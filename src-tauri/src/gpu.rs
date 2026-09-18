//! GPU leída del DRM del kernel, sin nvidia-smi ni intel_gpu_top.
//!
//! El uso global sale de un contador del controlador cuando lo hay (xe,
//! amdgpu). El uso por proceso sale de /proc/<pid>/fdinfo, que es lo mismo
//! que leen nvtop y gnome-system-monitor.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Serialize)]
pub struct Gpu {
    nombre: String,
    controlador: String,
    uso: Option<f32>,
    frecuencia_mhz: Option<u32>,
    frecuencia_max_mhz: Option<u32>,
    vram_usada: Option<u64>,
    vram_total: Option<u64>,
    temperatura: Option<f32>,
}

#[derive(Default, Clone, Copy)]
pub struct UsoProceso {
    pub uso: f32,
    pub memoria: u64,
}

struct Tarjeta {
    /// /sys/class/drm/cardN
    ruta: PathBuf,
    /// Dirección PCI; fdinfo la repite en `drm-pdev` para decir de qué tarjeta es el cliente.
    pdev: String,
    nombre: String,
    controlador: String,
    inactivo_previo: Option<(Instant, u64)>,
}

/// Por motor, (tiempo usado, total del motor).
type Motores = HashMap<String, (u64, Option<u64>)>;

/// Contadores de un cliente DRM.
/// xe da ciclos con su total propio; los demás dan nanosegundos y el total es
/// el reloj de pared.
#[derive(Default)]
struct Cliente {
    pdev: Option<String>,
    id: Option<u64>,
    motores: Motores,
    memoria: u64,
}

pub struct Gpus {
    tarjetas: Vec<Tarjeta>,
    descriptores: HashMap<u32, Vec<String>>,
    escaneados: HashSet<u32>,
    ultimo_escaneo: Option<Instant>,
    previos: HashMap<(String, u64), (Instant, Motores)>,
    uso_por_tarjeta: HashMap<String, f32>,
}

/// Recorrer /proc/*/fd entero es lo caro (un readlink por descriptor abierto).
/// Los descriptores DRM de un proceso casi nunca cambian, así que se reescanea
/// todo cada 10 s y entre medias solo se miran los PID nuevos.
const REESCANEO: Duration = Duration::from_secs(10);

impl Gpus {
    pub fn new() -> Self {
        let mut tarjetas: Vec<Tarjeta> = fs::read_dir("/sys/class/drm")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_prefix("card"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter_map(|e| Tarjeta::new(e.path()))
            .collect();
        tarjetas.sort_by(|a, b| a.ruta.cmp(&b.ruta));
        Gpus {
            tarjetas,
            descriptores: HashMap::new(),
            escaneados: HashSet::new(),
            ultimo_escaneo: None,
            previos: HashMap::new(),
            uso_por_tarjeta: HashMap::new(),
        }
    }

    pub fn leer(&mut self) -> Vec<Gpu> {
        let ahora = Instant::now();
        let uso_clientes = &self.uso_por_tarjeta;
        self.tarjetas
            .iter_mut()
            .map(|t| {
                let dev = t.ruta.join("device");
                let uso = match t.controlador.as_str() {
                    "xe" => t.uso_xe(ahora),
                    "amdgpu" => leer_num(&dev.join("gpu_busy_percent")),
                    // Sin contador global: la suma de clientes, que solo se
                    // conoce mientras la sección de GPU está abierta.
                    _ => uso_clientes.get(&t.pdev).copied(),
                };
                let (frecuencia_mhz, frecuencia_max_mhz) = match t.controlador.as_str() {
                    "xe" => (
                        leer_num(&dev.join("tile0/gt0/freq0/act_freq")),
                        leer_num(&dev.join("tile0/gt0/freq0/max_freq")),
                    ),
                    "i915" => (
                        leer_num(&t.ruta.join("gt_act_freq_mhz")),
                        leer_num(&t.ruta.join("gt_max_freq_mhz")),
                    ),
                    _ => (None, None),
                };
                Gpu {
                    nombre: t.nombre.clone(),
                    controlador: t.controlador.clone(),
                    uso,
                    frecuencia_mhz,
                    frecuencia_max_mhz,
                    vram_usada: leer_num(&dev.join("mem_info_vram_used")),
                    vram_total: leer_num(&dev.join("mem_info_vram_total")),
                    temperatura: temperatura(&dev),
                }
            })
            .collect()
    }

    /// Uso de GPU de cada PID de `pids` que tenga un cliente DRM abierto.
    pub fn por_proceso(&mut self, pids: &[u32]) -> HashMap<u32, UsoProceso> {
        let ahora = Instant::now();
        let completo = self
            .ultimo_escaneo
            .is_none_or(|t| ahora.duration_since(t) >= REESCANEO);
        if completo {
            self.descriptores.clear();
            self.escaneados.clear();
            self.ultimo_escaneo = Some(ahora);
        }
        for &pid in pids {
            if self.escaneados.insert(pid) {
                let fds = descriptores_drm(pid);
                if !fds.is_empty() {
                    self.descriptores.insert(pid, fds);
                }
            }
        }
        let vivos: HashSet<u32> = pids.iter().copied().collect();
        self.descriptores.retain(|pid, _| vivos.contains(pid));

        let mut previos = std::mem::take(&mut self.previos);
        let mut resultado: HashMap<u32, UsoProceso> = HashMap::new();
        let mut por_tarjeta: HashMap<String, f32> = HashMap::new();
        for (pid, fds) in &self.descriptores {
            for fd in fds {
                let ruta = format!("/proc/{pid}/fdinfo/{fd}");
                let Some(cliente) = fs::read_to_string(ruta)
                    .ok()
                    .and_then(|t| parsear_fdinfo(&t))
                else {
                    continue;
                };
                let pdev = cliente
                    .pdev
                    .or_else(|| self.tarjetas.first().map(|t| t.pdev.clone()))
                    .unwrap_or_default();
                // Un mismo cliente aparece en todos los descriptores que lo
                // comparten (dup, o pasado por un socket): se cuenta una vez.
                let clave = (pdev.clone(), cliente.id.unwrap_or(u64::MAX));
                if self.previos.contains_key(&clave) {
                    continue;
                }
                let uso = previos
                    .remove(&clave)
                    .map(|(antes, motores)| uso_cliente(&cliente.motores, &motores, ahora - antes))
                    .unwrap_or(0.0);
                let entrada = resultado.entry(*pid).or_default();
                entrada.uso += uso;
                entrada.memoria += cliente.memoria;
                *por_tarjeta.entry(pdev).or_default() += uso;
                self.previos.insert(clave, (ahora, cliente.motores));
            }
        }
        for uso in por_tarjeta.values_mut() {
            *uso = uso.min(100.0);
        }
        self.uso_por_tarjeta = por_tarjeta;
        resultado
    }
}

impl Tarjeta {
    fn new(ruta: PathBuf) -> Option<Self> {
        let dev = ruta.join("device");
        let pdev = fs::canonicalize(&dev)
            .ok()?
            .file_name()?
            .to_str()?
            .to_string();
        let uevent = fs::read_to_string(dev.join("uevent")).ok()?;
        let controlador = uevent
            .lines()
            .find_map(|l| l.strip_prefix("DRIVER="))?
            .to_string();
        let id = |f: &str| {
            fs::read_to_string(dev.join(f))
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        };
        let nombre = id("vendor")
            .zip(id("device"))
            .and_then(|(v, d)| nombre_pci(v, d))
            .unwrap_or_else(|| format!("GPU {controlador}"));
        Some(Tarjeta {
            ruta,
            pdev,
            nombre,
            controlador,
            inactivo_previo: None,
        })
    }

    /// xe cuenta el tiempo que la GT pasa en C6 (dormida). Lo que no es C6 es
    /// trabajo o espera breve antes de dormir.
    fn uso_xe(&mut self, ahora: Instant) -> Option<f32> {
        let ms: u64 = leer_num(&self.ruta.join("device/tile0/gt0/gtidle/idle_residency_ms"))?;
        let (antes, ms_antes) = self.inactivo_previo.replace((ahora, ms))?;
        let pared = ahora.duration_since(antes).as_secs_f64() * 1000.0;
        if pared <= 0.0 {
            return None;
        }
        let inactivo = ms.saturating_sub(ms_antes) as f64;
        Some(((1.0 - inactivo / pared).clamp(0.0, 1.0) * 100.0) as f32)
    }
}

fn leer_num<T: std::str::FromStr>(ruta: &Path) -> Option<T> {
    fs::read_to_string(ruta).ok()?.trim().parse().ok()
}

fn temperatura(dev: &Path) -> Option<f32> {
    let hwmon = fs::read_dir(dev.join("hwmon"))
        .ok()?
        .flatten()
        .next()?
        .path();
    leer_num::<f32>(&hwmon.join("temp1_input")).map(|m| m / 1000.0)
}

/// Nombre comercial desde pci.ids (hwdata). Se lee una vez por tarjeta al
/// arrancar: son 1,4 MB y evita depender de lspci.
fn nombre_pci(vendor: u16, device: u16) -> Option<String> {
    let texto = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"]
        .iter()
        .find_map(|r| fs::read_to_string(r).ok())?;
    buscar_pci(&texto, vendor, device)
}

pub(crate) fn buscar_pci(texto: &str, vendor: u16, device: u16) -> Option<String> {
    let v = format!("{vendor:04x} ");
    let d = format!("\t{device:04x} ");
    let mut lineas = texto.lines().skip_while(|l| !l.starts_with(&v));
    let fabricante = lineas.next()?[v.len()..].trim();
    // «Advanced Micro Devices, Inc. [AMD/ATI]» → AMD/ATI; «Intel Corporation» → Intel.
    let fabricante = match fabricante.split_once('[') {
        Some((_, resto)) => resto.trim_end_matches(']'),
        None => fabricante.split_whitespace().next().unwrap_or(fabricante),
    };
    let modelo = lineas
        .take_while(|l| l.starts_with('\t') || l.starts_with('#') || l.is_empty())
        .find_map(|l| l.strip_prefix(&d))?
        .trim();
    // «Lunar Lake [Intel Arc Graphics 130V / 140V]»: lo de dentro de los
    // corchetes es el nombre que conoce la gente; lo de fuera, el código interno.
    Some(match modelo.split_once('[') {
        Some((_, resto)) => resto.trim_end_matches(']').trim().to_string(),
        None => format!("{fabricante} {modelo}"),
    })
}

fn descriptores_drm(pid: u32) -> Vec<String> {
    let Ok(entradas) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    entradas
        .flatten()
        .filter(|e| fs::read_link(e.path()).is_ok_and(|destino| destino.starts_with("/dev/dri/")))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

fn parsear_fdinfo(texto: &str) -> Option<Cliente> {
    let mut cliente = Cliente::default();
    let mut es_drm = false;
    let (mut residente, mut memoria) = (0u64, 0u64);
    for linea in texto.lines() {
        let Some((clave, valor)) = linea.split_once(':') else {
            continue;
        };
        let valor = valor.trim();
        let numero = || valor.split_whitespace().next()?.parse::<u64>().ok();
        match clave {
            "drm-driver" => es_drm = true,
            "drm-pdev" => cliente.pdev = Some(valor.to_string()),
            "drm-client-id" => cliente.id = numero(),
            _ => {
                if let Some(motor) = clave.strip_prefix("drm-total-cycles-") {
                    cliente.motores.entry(motor.to_string()).or_default().1 = numero();
                } else if let Some(motor) = clave.strip_prefix("drm-cycles-") {
                    cliente.motores.entry(motor.to_string()).or_default().0 = numero()?;
                } else if let Some(motor) = clave.strip_prefix("drm-engine-") {
                    // i915 añade «drm-engine-capacity-<motor>», que no es tiempo.
                    if !motor.starts_with("capacity-") {
                        cliente.motores.entry(motor.to_string()).or_default().0 = numero()?;
                    }
                } else if clave.starts_with("drm-resident-") {
                    residente += bytes(valor);
                } else if clave.starts_with("drm-memory-") {
                    memoria += bytes(valor);
                }
            }
        }
    }
    // Los controladores nuevos dan residente por región; los viejos solo
    // drm-memory-*. Si están los dos, sumarlos contaría doble.
    cliente.memoria = if residente > 0 { residente } else { memoria };
    es_drm.then_some(cliente)
}

fn bytes(valor: &str) -> u64 {
    let mut partes = valor.split_whitespace();
    let n: u64 = partes.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    match partes.next() {
        Some("KiB") => n << 10,
        Some("MiB") => n << 20,
        Some("GiB") => n << 30,
        _ => n,
    }
}

/// Porcentaje del motor más ocupado. No se suman motores: render y vídeo a la
/// vez no son «200 %» de la GPU.
fn uso_cliente(ahora: &Motores, antes: &Motores, pared: Duration) -> f32 {
    let pared_ns = pared.as_nanos() as f64;
    ahora
        .iter()
        .filter_map(|(motor, &(usado, total))| {
            let &(usado0, total0) = antes.get(motor)?;
            let delta = usado.saturating_sub(usado0) as f64;
            let base = match (total, total0) {
                (Some(t), Some(t0)) => t.saturating_sub(t0) as f64,
                _ => pared_ns,
            };
            (base > 0.0).then(|| (delta / base * 100.0).min(100.0))
        })
        .fold(0.0, f64::max) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    // /proc/<firefox>/fdinfo/24 con el controlador xe, recortado.
    const XE: &str = "pos:\t0\nflags:\t02100002\ndrm-driver:\txe\ndrm-client-id:\t51\n\
drm-pdev:\t0000:00:02.0\ndrm-total-system:\t0\ndrm-resident-system:\t0\n\
drm-total-gtt:\t338580 KiB\ndrm-resident-gtt:\t338580 KiB\n\
drm-cycles-rcs:\t5419437458\ndrm-total-cycles-rcs:\t985599910098\n\
drm-cycles-bcs:\t59879\ndrm-total-cycles-bcs:\t985599910098\n";

    #[test]
    fn fdinfo_xe_ciclos_y_memoria() {
        let c = parsear_fdinfo(XE).expect("es un cliente DRM");
        assert_eq!(c.id, Some(51));
        assert_eq!(c.pdev.as_deref(), Some("0000:00:02.0"));
        assert_eq!(c.memoria, 338580 << 10);
        assert_eq!(c.motores["rcs"], (5419437458, Some(985599910098)));
    }

    #[test]
    fn fdinfo_i915_ignora_capacidad_y_usa_nanosegundos() {
        let texto = "drm-driver:\ti915\ndrm-client-id:\t7\ndrm-engine-render:\t2000000000 ns\n\
drm-engine-capacity-render:\t2\ndrm-memory-system:\t1024 KiB\n";
        let c = parsear_fdinfo(texto).expect("es un cliente DRM");
        assert_eq!(c.motores.len(), 1);
        assert_eq!(c.memoria, 1 << 20);
        let antes = HashMap::from([("render".to_string(), (1_500_000_000, None))]);
        let uso = uso_cliente(&c.motores, &antes, Duration::from_secs(1));
        assert!((uso - 50.0).abs() < 0.01);
    }

    #[test]
    fn fdinfo_que_no_es_drm() {
        assert!(parsear_fdinfo("pos:\t0\nflags:\t02\n").is_none());
    }

    #[test]
    fn uso_xe_con_ciclos_propios() {
        let antes = HashMap::from([("rcs".to_string(), (100, Some(1000)))]);
        let ahora = HashMap::from([("rcs".to_string(), (350, Some(2000)))]);
        let uso = uso_cliente(&ahora, &antes, Duration::from_secs(2));
        assert!((uso - 25.0).abs() < 0.01);
    }

    #[test]
    fn pci_ids_prefiere_nombre_comercial() {
        let ids = "8086  Intel Corporation\n\t7d51  Lunar Lake [Intel Arc Graphics 130V / 140V]\n\
\t\t8086 0001  Algo\n8087  Otro\n\t7d51  No es este\n";
        assert_eq!(
            buscar_pci(ids, 0x8086, 0x7d51).as_deref(),
            Some("Intel Arc Graphics 130V / 140V")
        );
        assert_eq!(buscar_pci(ids, 0x8086, 0x1234), None);
        // Línea real de hwdata para la GPU de un Galaxy Book5 Pro: sin corchetes.
        let lunar = "8086  Intel Corporation\n\t64a0  Core Ultra 200V Series Processors Arc Graphics 130V/140V GPU\n";
        assert_eq!(
            buscar_pci(lunar, 0x8086, 0x64a0).as_deref(),
            Some("Intel Core Ultra 200V Series Processors Arc Graphics 130V/140V GPU")
        );
    }
}
