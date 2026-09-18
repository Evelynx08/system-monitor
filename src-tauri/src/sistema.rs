//! CPU, memoria, disco y red.
//!
//! CPU y memoria se leen directamente de /proc: sysinfo no separa usuario de
//! sistema ni da la caché, y leer dos ficheros es más barato que su refresco.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use sysinfo::{DiskRefreshKind, Disks, Networks};

#[derive(Serialize)]
pub struct Cpu {
    modelo: String,
    uso: f32,
    usuario: f32,
    sistema: f32,
    nucleos: Vec<f32>,
    frecuencia_mhz: Option<u32>,
    temperatura: Option<f32>,
    carga: [f32; 3],
    procesos: usize,
    hilos: usize,
    activo_seg: u64,
}

#[derive(Serialize)]
pub struct Ram {
    total: u64,
    usada: u64,
    disponible: u64,
    cache: u64,
    comprimida: Option<u64>,
    swap_total: u64,
    swap_usada: u64,
    presion: Option<f32>,
}

#[derive(Serialize)]
pub struct Disco {
    lectura: u64,
    escritura: u64,
    lecturas: u32,
    escrituras: u32,
    volumenes: Vec<Volumen>,
}

#[derive(Serialize)]
pub struct Volumen {
    punto_montaje: String,
    dispositivo: String,
    sistema_archivos: String,
    total: u64,
    disponible: u64,
}

#[derive(Serialize)]
pub struct Red {
    bajada: u64,
    subida: u64,
    total_bajada: u64,
    total_subida: u64,
    interfaces: Vec<Interfaz>,
}

#[derive(Serialize)]
pub struct Interfaz {
    nombre: String,
    ip: Option<String>,
    mac: Option<String>,
    bajada: u64,
    subida: u64,
    total_bajada: u64,
    total_subida: u64,
    es_virtual: bool,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Tiempos {
    usuario: u64,
    sistema: u64,
    total: u64,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct ContadoresDisco {
    sectores_leidos: u64,
    sectores_escritos: u64,
    lecturas: u64,
    escrituras: u64,
}

pub struct Sistema {
    modelo: String,
    sensor: Option<PathBuf>,
    cpu_previo: Vec<Tiempos>,
    disco_previo: Option<(Instant, ContadoresDisco)>,
    red: Networks,
    red_instante: Instant,
    discos: Disks,
}

impl Sistema {
    pub fn new() -> Self {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let modelo = cpuinfo
            .lines()
            .find_map(|l| {
                let (clave, valor) = l.split_once(':')?;
                matches!(clave.trim(), "model name" | "Hardware" | "Model").then(|| valor.trim())
            })
            .unwrap_or_default()
            .to_string();
        Sistema {
            modelo,
            sensor: sensor_cpu(),
            cpu_previo: Vec::new(),
            disco_previo: None,
            red: Networks::new_with_refreshed_list(),
            red_instante: Instant::now(),
            discos: Disks::new(),
        }
    }

    pub fn cpu(&mut self) -> Cpu {
        let stat = fs::read_to_string("/proc/stat").expect("/proc/stat existe en Linux");
        let actual = tiempos_cpu(&stat);
        let porcentajes: Vec<(f32, f32)> = actual
            .iter()
            .zip(
                self.cpu_previo
                    .iter()
                    .chain(std::iter::repeat(&Tiempos::default())),
            )
            .map(|(a, b)| porcentaje_cpu(a, b))
            .collect();
        // Sin muestra anterior el delta es todo el tiempo desde el arranque:
        // mejor 0 que una media de horas.
        let porcentajes = if self.cpu_previo.is_empty() {
            vec![(0.0, 0.0); actual.len()]
        } else {
            porcentajes
        };
        self.cpu_previo = actual;

        let (usuario, sistema) = porcentajes.first().copied().unwrap_or_default();
        let nucleos: Vec<f32> = porcentajes.iter().skip(1).map(|(u, s)| u + s).collect();

        let frecuencias: Vec<u32> = (0..nucleos.len())
            .filter_map(|i| {
                leer_num::<u32>(&PathBuf::from(format!(
                    "/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"
                )))
            })
            .collect();
        let frecuencia_mhz = (!frecuencias.is_empty())
            .then(|| frecuencias.iter().sum::<u32>() / frecuencias.len() as u32 / 1000);

        let loadavg = fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let campos: Vec<&str> = loadavg.split_whitespace().collect();
        let carga = [0, 1, 2].map(|i| campos.get(i).and_then(|c| c.parse().ok()).unwrap_or(0.0));
        let hilos = campos
            .get(3)
            .and_then(|c| c.split_once('/'))
            .and_then(|(_, total)| total.parse().ok())
            .unwrap_or(0);
        let procesos = fs::read_dir("/proc")
            .map(|d| {
                d.flatten()
                    .filter(|e| {
                        e.file_name()
                            .as_encoded_bytes()
                            .iter()
                            .all(u8::is_ascii_digit)
                    })
                    .count()
            })
            .unwrap_or(0);
        let activo_seg = fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|u| u.split('.').next()?.parse().ok())
            .unwrap_or(0);

        Cpu {
            modelo: self.modelo.clone(),
            uso: usuario + sistema,
            usuario,
            sistema,
            nucleos,
            frecuencia_mhz,
            temperatura: self
                .sensor
                .as_deref()
                .and_then(leer_num::<f32>)
                .map(|m| m / 1000.0),
            carga,
            procesos,
            hilos,
            activo_seg,
        }
    }

    pub fn disco(&mut self) -> Disco {
        let ahora = Instant::now();
        let texto = fs::read_to_string("/proc/diskstats").expect("/proc/diskstats existe en Linux");
        let actual = contadores_disco(&texto, |n| Path::new("/sys/block").join(n).exists());
        let (mut lectura, mut escritura, mut lecturas, mut escrituras) = (0, 0, 0, 0);
        if let Some((antes, previo)) = self.disco_previo.replace((ahora, actual)) {
            let seg = ahora.duration_since(antes).as_secs_f64();
            if seg > 0.0 {
                let ritmo = |a: u64, b: u64| a.saturating_sub(b) as f64 / seg;
                // Los sectores de diskstats son siempre de 512 bytes, sea cual
                // sea el tamaño de sector real del disco.
                lectura = (ritmo(actual.sectores_leidos, previo.sectores_leidos) * 512.0) as u64;
                escritura =
                    (ritmo(actual.sectores_escritos, previo.sectores_escritos) * 512.0) as u64;
                lecturas = ritmo(actual.lecturas, previo.lecturas).round() as u32;
                escrituras = ritmo(actual.escrituras, previo.escrituras).round() as u32;
            }
        }

        self.discos
            .refresh_specifics(true, DiskRefreshKind::nothing().with_storage());
        // En btrfs cada subvolumen montado aparece como un disco con el mismo
        // dispositivo y el mismo espacio: se deja el punto de montaje más corto.
        let mut por_dispositivo: HashMap<&std::ffi::OsStr, &sysinfo::Disk> = HashMap::new();
        for d in self.discos.list().iter().filter(|d| d.total_space() > 0) {
            por_dispositivo
                .entry(d.name())
                .and_modify(|previo| {
                    if d.mount_point().as_os_str().len() < previo.mount_point().as_os_str().len() {
                        *previo = d;
                    }
                })
                .or_insert(d);
        }
        let mut volumenes: Vec<Volumen> = por_dispositivo
            .into_values()
            .map(|d| Volumen {
                punto_montaje: d.mount_point().to_string_lossy().into_owned(),
                dispositivo: d.name().to_string_lossy().into_owned(),
                sistema_archivos: d.file_system().to_string_lossy().into_owned(),
                total: d.total_space(),
                disponible: d.available_space(),
            })
            .collect();
        volumenes.sort_by(|a, b| a.punto_montaje.cmp(&b.punto_montaje));

        Disco {
            lectura,
            escritura,
            lecturas,
            escrituras,
            volumenes,
        }
    }

    pub fn red(&mut self) -> Red {
        let ahora = Instant::now();
        let seg = ahora.duration_since(self.red_instante).as_secs_f64();
        self.red_instante = ahora;
        self.red.refresh(true);
        let ritmo = |bytes: u64| {
            if seg > 0.0 {
                (bytes as f64 / seg) as u64
            } else {
                0
            }
        };

        let mut interfaces: Vec<Interfaz> = self
            .red
            .list()
            .iter()
            .filter(|(nombre, _)| *nombre != "lo")
            .map(|(nombre, d)| {
                let ip = d
                    .ip_networks()
                    .iter()
                    .map(|n| n.addr)
                    .min_by_key(|a| match a {
                        std::net::IpAddr::V4(_) => 0,
                        std::net::IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => 2,
                        std::net::IpAddr::V6(_) => 1,
                    })
                    .map(|a| a.to_string());
                Interfaz {
                    nombre: nombre.clone(),
                    ip,
                    mac: (!d.mac_address().is_unspecified()).then(|| d.mac_address().to_string()),
                    bajada: ritmo(d.received()),
                    subida: ritmo(d.transmitted()),
                    total_bajada: d.total_received(),
                    total_subida: d.total_transmitted(),
                    es_virtual: es_virtual(nombre),
                }
            })
            .collect();
        interfaces.sort_by(|a, b| {
            (a.es_virtual, a.ip.is_none(), &a.nombre).cmp(&(
                b.es_virtual,
                b.ip.is_none(),
                &b.nombre,
            ))
        });

        // Las virtuales no suman: el tráfico de una VPN o de un contenedor ya
        // pasa por la interfaz física y se contaría dos veces.
        let fisicas = || interfaces.iter().filter(|i| !i.es_virtual);
        Red {
            bajada: fisicas().map(|i| i.bajada).sum(),
            subida: fisicas().map(|i| i.subida).sum(),
            total_bajada: fisicas().map(|i| i.total_bajada).sum(),
            total_subida: fisicas().map(|i| i.total_subida).sum(),
            interfaces,
        }
    }
}

pub fn ram() -> Ram {
    let texto = fs::read_to_string("/proc/meminfo").expect("/proc/meminfo existe en Linux");
    let m = meminfo(&texto);
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let total = g("MemTotal");
    let disponible = g("MemAvailable");
    Ram {
        total,
        usada: total.saturating_sub(disponible),
        disponible,
        // Shmem cuenta dentro de Cached pero no se puede liberar: no es caché.
        cache: (g("Cached") + g("Buffers") + g("SReclaimable")).saturating_sub(g("Shmem")),
        comprimida: m.get("Zswap").copied(),
        swap_total: g("SwapTotal"),
        swap_usada: g("SwapTotal").saturating_sub(g("SwapFree")),
        presion: presion_memoria(),
    }
}

fn leer_num<T: std::str::FromStr>(ruta: &Path) -> Option<T> {
    fs::read_to_string(ruta).ok()?.trim().parse().ok()
}

fn tiempos_cpu(stat: &str) -> Vec<Tiempos> {
    stat.lines()
        .take_while(|l| l.starts_with("cpu"))
        .map(|l| {
            let v: Vec<u64> = l
                .split_ascii_whitespace()
                .skip(1)
                .filter_map(|x| x.parse().ok())
                .collect();
            let campo = |i: usize| v.get(i).copied().unwrap_or(0);
            // user nice system idle iowait irq softirq steal [guest guest_nice]
            // guest y guest_nice ya van incluidos en user y nice.
            let usuario = campo(0) + campo(1);
            let sistema = campo(2) + campo(5) + campo(6) + campo(7);
            let inactivo = campo(3) + campo(4);
            Tiempos {
                usuario,
                sistema,
                total: usuario + sistema + inactivo,
            }
        })
        .collect()
}

fn porcentaje_cpu(ahora: &Tiempos, antes: &Tiempos) -> (f32, f32) {
    let total = ahora.total.saturating_sub(antes.total) as f32;
    if total == 0.0 {
        return (0.0, 0.0);
    }
    (
        ahora.usuario.saturating_sub(antes.usuario) as f32 / total * 100.0,
        ahora.sistema.saturating_sub(antes.sistema) as f32 / total * 100.0,
    )
}

fn meminfo(texto: &str) -> HashMap<&str, u64> {
    texto
        .lines()
        .filter_map(|l| {
            let (clave, valor) = l.split_once(':')?;
            let kb: u64 = valor.split_whitespace().next()?.parse().ok()?;
            Some((clave, kb * 1024))
        })
        .collect()
}

/// `some avg10`: porcentaje de los últimos 10 s en que algún proceso esperó
/// por memoria. Es lo más parecido a la «presión de memoria» de macOS.
fn presion_memoria() -> Option<f32> {
    let texto = fs::read_to_string("/proc/pressure/memory").ok()?;
    texto
        .lines()
        .find(|l| l.starts_with("some"))?
        .split_whitespace()
        .find_map(|c| c.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// Suma solo discos enteros. Las particiones repetirían lo de su disco, y los
/// device-mapper y md lo de los discos que tienen debajo; zram es memoria.
fn contadores_disco(texto: &str, es_bloque: impl Fn(&str) -> bool) -> ContadoresDisco {
    texto
        .lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            let nombre = *c.get(2)?;
            let descartado = ["loop", "ram", "zram", "dm-", "md", "sr", "fd"]
                .iter()
                .any(|p| nombre.starts_with(p));
            if descartado || !es_bloque(nombre) {
                return None;
            }
            let n = |i: usize| c.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
            Some(ContadoresDisco {
                lecturas: n(3),
                sectores_leidos: n(5),
                escrituras: n(7),
                sectores_escritos: n(9),
            })
        })
        .fold(ContadoresDisco::default(), |a, b| ContadoresDisco {
            sectores_leidos: a.sectores_leidos + b.sectores_leidos,
            sectores_escritos: a.sectores_escritos + b.sectores_escritos,
            lecturas: a.lecturas + b.lecturas,
            escrituras: a.escrituras + b.escrituras,
        })
}

fn es_virtual(nombre: &str) -> bool {
    [
        "tailscale",
        "docker",
        "veth",
        "br-",
        "virbr",
        "tun",
        "tap",
        "wg",
        "vpn",
        "vboxnet",
        "vmnet",
        "podman",
        "lxc",
        "zt",
    ]
    .iter()
    .any(|p| nombre.starts_with(p))
}

fn sensor_cpu() -> Option<PathBuf> {
    // Del más fiable al que menos: coretemp y k10temp miden el encapsulado;
    // acpitz es una zona térmica genérica que a veces ni es la CPU.
    const NOMBRES: [&str; 5] = ["coretemp", "k10temp", "zenpower", "cpu_thermal", "acpitz"];
    fs::read_dir("/sys/class/hwmon")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let nombre = fs::read_to_string(e.path().join("name")).ok()?;
            let prioridad = NOMBRES.iter().position(|n| *n == nombre.trim())?;
            let ruta = e.path().join("temp1_input");
            ruta.exists().then_some((prioridad, ruta))
        })
        .min_by_key(|(p, _)| *p)
        .map(|(_, ruta)| ruta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_separa_usuario_y_sistema() {
        let antes =
            tiempos_cpu("cpu  100 0 50 850 0 0 0 0 0 0\ncpu0 100 0 50 850 0 0 0 0 0 0\nintr 1\n");
        let ahora =
            tiempos_cpu("cpu  160 20 70 1030 10 5 5 0 0 0\ncpu0 160 20 70 1030 10 5 5 0 0 0\n");
        assert_eq!(ahora.len(), 2);
        // Δ usuario 80, Δ sistema 30, Δ inactivo 190 → total 300
        let (u, s) = porcentaje_cpu(&ahora[0], &antes[0]);
        assert!((u - 26.67).abs() < 0.01);
        assert!((s - 10.0).abs() < 0.01);
    }

    #[test]
    fn diskstats_solo_discos_enteros() {
        let texto = "\
 259       0 nvme0n1 330087 161586 36011881 442538 1412903 259855 82264874 5271149 0 655785 5786569
 259       1 nvme0n1p1 276 744 11663 79 2 0 2 34 0 67 114
 252       0 zram0 100 0 800 0 200 0 1600 0 0 0 0
 253       0 dm-0 50 0 400 0 60 0 480 0 0 0 0
";
        let c = contadores_disco(texto, |n| !n.contains('p'));
        assert_eq!(
            c,
            ContadoresDisco {
                sectores_leidos: 36011881,
                sectores_escritos: 82264874,
                lecturas: 330087,
                escrituras: 1412903,
            }
        );
    }

    #[test]
    fn meminfo_en_bytes() {
        let m = meminfo(
            "MemTotal:       32000000 kB\nMemAvailable:   16000000 kB\nHugePages_Total:       0\n",
        );
        assert_eq!(m["MemTotal"], 32000000 * 1024);
        assert_eq!(m["HugePages_Total"], 0);
    }
}
