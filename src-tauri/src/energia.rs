//! Perfiles de energía, historial de la batería y predicción de autonomía.
//!
//! - Perfiles: power-profiles-daemon (y tuned-ppd, que expone la misma API)
//!   por D-Bus con `gdbus`, que viene con GLib. Cambiar de perfil no pide
//!   contraseña: su política de polkit lo permite al usuario de la sesión activa.
//! - Historial: los ficheros que UPower escribe en /var/lib/upower, legibles
//!   por cualquiera y que cubren también las horas con el monitor cerrado.
//! - Predicción por perfil: nadie guarda qué perfil había en cada momento, así
//!   que el monitor apunta lo que gasta el equipo con cada perfil mientras está
//!   abierto y con batería, y lo conserva en disco entre sesiones.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::bateria::Bateria;

const DIR_UPOWER: &str = "/var/lib/upower";
const ESTADO_PPD: &str = "/var/lib/power-profiles-daemon/state.ini";

const DESTINOS: [(&str, &str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    ),
    // Nombre anterior a power-profiles-daemon 0.20 (Debian 12, Ubuntu 22.04).
    (
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
    ),
];

/// Lo medido con un perfil hace 20 h de uso con batería pesa la mitad que lo
/// de ahora: se adapta si cambia la forma de usar el equipo sin tirar lo
/// aprendido por una tarde rara.
const VIDA_MEDIA_S: f64 = 20.0 * 3600.0;
/// Menos de 10 min con un perfil es un par de picos, no una media.
const MINIMO_PERFIL_S: f64 = 10.0 * 60.0;
const VENTANA_RECIENTE: Duration = Duration::from_secs(10 * 60);
/// Con la ventana minimizada no se muestrea; un hueco mayor que esto no es
/// tiempo con batería que se haya medido.
const SALTO_MAXIMO_S: f64 = 30.0;
const BARRAS_HISTORIAL: usize = 180;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Perfiles {
    activo: String,
    disponibles: Vec<String>,
    /// Motivo por el que el demonio rebaja el rendimiento, p. ej.
    /// «lap-detected» o «high-operating-temperature».
    degradado: Option<String>,
}

#[derive(Serialize)]
pub struct Estimacion {
    vatios: f32,
    minutos: Option<u32>,
}

#[derive(Serialize)]
pub struct EstimacionPerfil {
    perfil: String,
    vatios: f32,
    minutos: Option<u32>,
    medido_min: u32,
}

#[derive(Serialize)]
pub struct EstadoEnergia {
    perfiles: Option<Perfiles>,
    reciente: Option<Estimacion>,
    habitual: Option<Estimacion>,
    por_perfil: Vec<EstimacionPerfil>,
}

#[derive(Serialize)]
pub struct Historial {
    desde: i64,
    hasta: i64,
    carga: Vec<Option<f32>>,
    potencia: Vec<Option<f32>>,
    cargando: Vec<bool>,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug)]
struct Acumulado {
    julios: f64,
    segundos: f64,
    /// Sin olvido: lo que se enseña como «medido durante».
    segundos_total: f64,
}

impl Acumulado {
    fn sumar(&mut self, vatios: f64, dt: f64) {
        let olvido = 0.5f64.powf(dt / VIDA_MEDIA_S);
        self.julios = self.julios * olvido + vatios * dt;
        self.segundos = self.segundos * olvido + dt;
        self.segundos_total += dt;
    }

    fn vatios(&self) -> Option<f64> {
        (self.segundos > 0.0).then(|| self.julios / self.segundos)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum EstadoUp {
    Cargando,
    Descargando,
    /// Llena, en pausa por el límite de carga o esperando: el valor no cambia.
    Estable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Punto {
    t: i64,
    valor: f32,
    estado: EstadoUp,
}

struct Upower {
    potencia: Vec<Punto>,
    carga: Vec<Punto>,
    modificado: Option<SystemTime>,
}

pub struct Energia {
    perfiles: Option<Perfiles>,
    perfiles_leidos: Option<Instant>,
    recientes: VecDeque<(Instant, f32)>,
    ultima_descarga: Option<Instant>,
    registro: HashMap<String, Acumulado>,
    ruta_registro: Option<PathBuf>,
    pendiente_guardar: bool,
    guardado: Instant,
    upower: Option<Upower>,
    upower_revisado: Option<Instant>,
}

impl Energia {
    pub fn new() -> Self {
        let ruta_registro = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
            .map(|d| d.join("bookos-system-monitor/consumo.json"));
        // Un fichero ilegible o de otra versión solo cuesta volver a aprender.
        let registro = ruta_registro
            .as_ref()
            .and_then(|r| fs::read_to_string(r).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Energia {
            perfiles: None,
            perfiles_leidos: None,
            recientes: VecDeque::new(),
            ultima_descarga: None,
            registro,
            ruta_registro,
            pendiente_guardar: false,
            guardado: Instant::now(),
            upower: None,
            upower_revisado: None,
        }
    }

    pub fn muestra(&mut self, bateria: Option<&Bateria>) -> EstadoEnergia {
        let ahora = Instant::now();
        self.actualizar_perfiles(ahora);
        self.revisar_upower(ahora);

        match bateria
            .filter(|b| b.descargando())
            .and_then(Bateria::potencia_w)
        {
            Some(w) => {
                let dt = self
                    .ultima_descarga
                    .map(|t| ahora.duration_since(t).as_secs_f64());
                if let (Some(dt), Some(p)) = (dt.filter(|d| *d <= SALTO_MAXIMO_S), &self.perfiles) {
                    self.registro
                        .entry(p.activo.clone())
                        .or_default()
                        .sumar(f64::from(w), dt);
                    self.pendiente_guardar = true;
                }
                self.ultima_descarga = Some(ahora);
                self.recientes.push_back((ahora, w));
            }
            None => self.ultima_descarga = None,
        }
        while self
            .recientes
            .front()
            .is_some_and(|(t, _)| ahora.duration_since(*t) > VENTANA_RECIENTE)
        {
            self.recientes.pop_front();
        }
        if self.pendiente_guardar && self.guardado.elapsed() >= Duration::from_secs(60) {
            self.guardar();
        }

        let energia_wh = bateria.and_then(Bateria::energia_wh).map(f64::from);
        let estimar = |w: f64| Estimacion {
            vatios: w as f32,
            minutos: energia_wh
                .filter(|_| w > 0.0)
                .map(|e| (e / w * 60.0).round() as u32),
        };
        let unix = ahora_unix();
        let reciente = self.consumo_reciente(ahora, unix).map(estimar);
        let habitual = self
            .upower
            .as_ref()
            .and_then(|u| consumo_medio(&u.potencia, unix, 7 * 24 * 3600, 30 * 60))
            .map(estimar);
        let por_perfil = self
            .perfiles
            .iter()
            .flat_map(|p| &p.disponibles)
            .filter_map(|nombre| {
                let a = self.registro.get(nombre)?;
                if a.segundos_total < MINIMO_PERFIL_S {
                    return None;
                }
                let e = estimar(a.vatios()?);
                Some(EstimacionPerfil {
                    perfil: nombre.clone(),
                    vatios: e.vatios,
                    minutos: e.minutos,
                    medido_min: (a.segundos_total / 60.0) as u32,
                })
            })
            .collect();

        EstadoEnergia {
            perfiles: self.perfiles.clone(),
            reciente,
            habitual,
            por_perfil,
        }
    }

    pub fn invalidar_perfiles(&mut self) {
        self.perfiles_leidos = None;
    }

    pub fn historial(&mut self, horas: u32) -> Option<Historial> {
        // Quien abre el historial quiere lo último que haya escrito UPower.
        self.upower_revisado = None;
        self.revisar_upower(Instant::now());
        let u = self.upower.as_ref()?;
        let hasta = ahora_unix();
        let desde = hasta - i64::from(horas.clamp(1, 24 * 30)) * 3600;
        Some(construir_historial(u, desde, hasta))
    }

    fn consumo_reciente(&self, ahora: Instant, unix: i64) -> Option<f64> {
        let cubre = self
            .recientes
            .front()
            .is_some_and(|(t, _)| ahora.duration_since(*t) >= Duration::from_secs(120));
        if cubre {
            let suma: f64 = self.recientes.iter().map(|(_, w)| f64::from(*w)).sum();
            return Some(suma / self.recientes.len() as f64);
        }
        // Recién abierto: lo que midió UPower en los últimos 10 min.
        let u = self.upower.as_ref()?;
        consumo_medio(&u.potencia, unix, VENTANA_RECIENTE.as_secs() as i64, 120)
    }

    fn actualizar_perfiles(&mut self, ahora: Instant) {
        // La lista de perfiles y el motivo de rendimiento rebajado cambian poco:
        // gdbus cada 30 s. El perfil activo puede cambiarse desde el panel en
        // cualquier momento y power-profiles-daemon lo deja en un fichero que
        // cuesta leer lo mismo que /proc.
        let caducado = self
            .perfiles_leidos
            .is_none_or(|t| ahora.duration_since(t) >= Duration::from_secs(30));
        if caducado {
            self.perfiles = leer_perfiles().map(|(p, _)| p);
            self.perfiles_leidos = Some(ahora);
        } else if let Some(p) = self.perfiles.as_mut() {
            if let Some(activo) = fs::read_to_string(ESTADO_PPD)
                .ok()
                .and_then(|t| perfil_de_estado(&t))
            {
                p.activo = activo;
            }
        }
    }

    fn revisar_upower(&mut self, ahora: Instant) {
        // UPower vuelca su historial a disco cada pocos minutos: mirar la fecha
        // del fichero una vez por minuto basta.
        if self
            .upower_revisado
            .is_some_and(|t| ahora.duration_since(t) < Duration::from_secs(60))
        {
            return;
        }
        self.upower_revisado = Some(ahora);
        let Some(ruta_potencia) = fichero_upower() else {
            self.upower = None;
            return;
        };
        let modificado = fs::metadata(&ruta_potencia).and_then(|m| m.modified()).ok();
        if modificado.is_some()
            && self
                .upower
                .as_ref()
                .is_some_and(|u| u.modificado == modificado)
        {
            return;
        }
        let nombre = ruta_potencia
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .replacen("history-rate-", "history-charge-", 1);
        let leer = |r: &Path| {
            fs::read_to_string(r)
                .map(|t| parsear_upower(&t))
                .unwrap_or_default()
        };
        self.upower = Some(Upower {
            potencia: leer(&ruta_potencia),
            carga: leer(&ruta_potencia.with_file_name(nombre)),
            modificado,
        });
    }

    fn guardar(&mut self) {
        self.pendiente_guardar = false;
        self.guardado = Instant::now();
        let Some(ruta) = &self.ruta_registro else {
            return;
        };
        let texto =
            serde_json::to_string(&self.registro).expect("un mapa de números siempre se serializa");
        let resultado = ruta
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(ruta, texto));
        if let Err(e) = resultado {
            eprintln!(
                "no se pudo guardar el consumo por perfil en {}: {e}",
                ruta.display()
            );
        }
    }
}

/// Devuelve la clave de traducción del error para el frontend.
pub fn cambiar_perfil(perfil: &str) -> Result<(), String> {
    let (actual, i) = leer_perfiles().ok_or("error.sin_perfiles")?;
    // Solo nombres que anuncia el propio demonio: el valor va dentro de un
    // literal GVariant y así no puede cerrarlo ni colar otra cosa.
    if !actual.disponibles.iter().any(|p| p == perfil) {
        return Err("error.perfil_desconocido".into());
    }
    let valor = format!("<'{perfil}'>");
    gdbus(
        DESTINOS[i],
        "org.freedesktop.DBus.Properties.Set",
        &[DESTINOS[i].2, "ActiveProfile", &valor],
    )
    .map(|_| ())
    .ok_or_else(|| "error.perfil_no_cambiado".into())
}

fn gdbus(destino: (&str, &str, &str), metodo: &str, argumentos: &[&str]) -> Option<String> {
    let salida = Command::new("gdbus")
        .args([
            "call",
            "--system",
            "--dest",
            destino.0,
            "--object-path",
            destino.1,
            "--method",
            metodo,
        ])
        .args(argumentos)
        .output()
        .ok()?;
    salida
        .status
        .success()
        .then(|| String::from_utf8_lossy(&salida.stdout).into_owned())
}

/// Perfiles y el índice en `DESTINOS` del nombre de bus que contestó.
fn leer_perfiles() -> Option<(Perfiles, usize)> {
    DESTINOS.iter().enumerate().find_map(|(i, d)| {
        let texto = gdbus(*d, "org.freedesktop.DBus.Properties.GetAll", &[d.2])?;
        parsear_perfiles(&texto).map(|p| (p, i))
    })
}

fn parsear_perfiles(texto: &str) -> Option<Perfiles> {
    let cadena = |clave: &str| {
        let resto = texto.split(&format!("'{clave}': <'")).nth(1)?;
        resto.split('\'').next().map(str::to_string)
    };
    Some(Perfiles {
        activo: cadena("ActiveProfile")?,
        disponibles: texto
            .split("'Profile': <'")
            .skip(1)
            .filter_map(|r| r.split('\'').next())
            .map(str::to_string)
            .collect(),
        degradado: cadena("PerformanceDegraded").filter(|d| !d.is_empty()),
    })
}

fn perfil_de_estado(texto: &str) -> Option<String> {
    let mut en_estado = false;
    texto.lines().map(str::trim).find_map(|l| {
        if l.starts_with('[') {
            en_estado = l == "[State]";
            return None;
        }
        l.strip_prefix("Profile=")
            .filter(|_| en_estado)
            .map(str::to_string)
    })
}

/// El de la batería del equipo. Ratones y auriculares también dejan ficheros
/// ahí, pero con permisos 640; de los legibles, el último que se escribió.
fn fichero_upower() -> Option<PathBuf> {
    fs::read_dir(DIR_UPOWER)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.starts_with("history-rate-") && n.ends_with(".dat") && !n.contains("generic_id")
            })
        })
        .filter(|e| fs::File::open(e.path()).is_ok())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn parsear_upower(texto: &str) -> Vec<Punto> {
    texto
        .lines()
        .filter_map(|l| {
            let mut campos = l.split('\t');
            let t = campos.next()?.trim().parse().ok()?;
            // UPower escribe el decimal con el locale del demonio: «81,000».
            let valor = campos.next()?.trim().replace(',', ".").parse().ok()?;
            let estado = match campos.next()?.trim() {
                "charging" => EstadoUp::Cargando,
                "discharging" => EstadoUp::Descargando,
                "unknown" => return None,
                _ => EstadoUp::Estable,
            };
            Some(Punto { t, valor, estado })
        })
        .collect()
}

fn ahora_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Potencia media mientras se descargaba, ponderada por tiempo: cada muestra
/// vale hasta la siguiente con un tope de 10 min. UPower escribe cada ~30 s
/// mientras algo cambia (medido: 2235 de 2279 intervalos ≤ 60 s), así que un
/// hueco mayor es el equipo suspendido o apagado, no consumo.
fn consumo_medio(puntos: &[Punto], ahora: i64, ventana: i64, minimo_s: i64) -> Option<f64> {
    let desde = ahora - ventana;
    let (julios, segundos) = puntos
        .iter()
        .zip(puntos.iter().skip(1).map(|s| s.t).chain([ahora]))
        .filter(|(p, _)| p.t >= desde && p.estado == EstadoUp::Descargando && p.valor > 0.0)
        .fold((0.0, 0i64), |(j, s), (p, siguiente)| {
            let dt = (siguiente - p.t).clamp(0, 600);
            (j + f64::from(p.valor) * dt as f64, s + dt)
        });
    (segundos >= minimo_s).then(|| julios / segundos as f64)
}

/// Valor vigente en `t`. UPower solo apunta cambios, así que la última muestra
/// sigue valiendo; pero si cargaba o descargaba y pasan 30 min sin otra, el
/// equipo estuvo apagado o suspendido y no se inventa la línea.
fn valor_en(puntos: &[Punto], t: i64) -> Option<Punto> {
    let i = puntos.partition_point(|p| p.t <= t);
    let p = *puntos.get(i.checked_sub(1)?)?;
    let limite = if p.estado == EstadoUp::Estable {
        24 * 3600
    } else {
        30 * 60
    };
    (t - p.t <= limite).then_some(p)
}

fn media_en(puntos: &[Punto], inicio: i64, fin: i64) -> Option<f32> {
    let tramo =
        &puntos[puntos.partition_point(|p| p.t < inicio)..puntos.partition_point(|p| p.t < fin)];
    if tramo.is_empty() {
        return valor_en(puntos, fin)
            .filter(|p| fin - p.t <= 600)
            .map(|p| p.valor);
    }
    Some(tramo.iter().map(|p| p.valor).sum::<f32>() / tramo.len() as f32)
}

fn construir_historial(u: &Upower, desde: i64, hasta: i64) -> Historial {
    let paso = (hasta - desde) as f64 / BARRAS_HISTORIAL as f64;
    let limite = |i: usize| desde + (i as f64 * paso) as i64;
    let mut h = Historial {
        desde,
        hasta,
        carga: Vec::with_capacity(BARRAS_HISTORIAL),
        potencia: Vec::with_capacity(BARRAS_HISTORIAL),
        cargando: Vec::with_capacity(BARRAS_HISTORIAL),
    };
    for (inicio, fin) in (0..BARRAS_HISTORIAL).map(|i| (limite(i), limite(i + 1))) {
        let carga = valor_en(&u.carga, fin);
        h.carga.push(carga.map(|p| p.valor));
        h.cargando
            .push(carga.is_some_and(|p| p.estado == EstadoUp::Cargando));
        h.potencia.push(media_en(&u.potencia, inicio, fin));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: fn(i64, f32, EstadoUp) -> Punto = |t, valor, estado| Punto { t, valor, estado };

    #[test]
    fn perfiles_desde_getall_de_power_profiles_daemon() {
        // Salida real de gdbus con power-profiles-daemon 0.30, recortada.
        let texto = "({'ActiveProfile': <'performance'>, 'PerformanceInhibited': <''>, \
            'PerformanceDegraded': <''>, 'Profiles': <[{'Profile': <'power-saver'>, \
            'CpuDriver': <'intel_pstate'>}, {'Profile': <'balanced'>, 'CpuDriver': <'intel_pstate'>}, \
            {'Profile': <'performance'>, 'Driver': <'multiple'>}]>, 'ActiveProfileHolds': <@aa{sv} []>},)";
        assert_eq!(
            parsear_perfiles(texto),
            Some(Perfiles {
                activo: "performance".into(),
                disponibles: vec![
                    "power-saver".into(),
                    "balanced".into(),
                    "performance".into()
                ],
                degradado: None,
            })
        );
        let caliente = texto.replace(
            "'PerformanceDegraded': <''>",
            "'PerformanceDegraded': <'lap-detected'>",
        );
        assert_eq!(
            parsear_perfiles(&caliente)
                .and_then(|p| p.degradado)
                .as_deref(),
            Some("lap-detected")
        );
    }

    #[test]
    fn perfil_activo_del_fichero_de_estado() {
        let texto = "[State]\nbattery_aware=true\nProfile=balanced\n\n[Actions]\nProfile=otro\n";
        assert_eq!(perfil_de_estado(texto).as_deref(), Some("balanced"));
    }

    #[test]
    fn upower_con_coma_decimal_y_sin_unknown() {
        let puntos = parsear_upower("1789071921\t81,000\tcharging\n1789084655\t0,000\tunknown\n1789504190\t80,000\tpending-charge\n");
        assert_eq!(
            puntos,
            vec![
                P(1789071921, 81.0, EstadoUp::Cargando),
                P(1789504190, 80.0, EstadoUp::Estable)
            ]
        );
    }

    #[test]
    fn consumo_ponderado_por_tiempo_ignora_carga_y_huecos() {
        let puntos = [
            P(0, 8.0, EstadoUp::Descargando),
            P(30, 12.0, EstadoUp::Descargando),
            P(60, 20.0, EstadoUp::Cargando),
            P(2000, 10.0, EstadoUp::Descargando),
        ];
        // 30 s·8 W + 30 s·12 W + 60 s·10 W = 1200 J en 120 s
        assert_eq!(consumo_medio(&puntos, 2060, 3600, 120), Some(10.0));
        // Un suspendido de horas no cuenta más de 10 min.
        let dormido = [
            P(0, 5.0, EstadoUp::Descargando),
            P(36_000, 15.0, EstadoUp::Descargando),
        ];
        assert_eq!(consumo_medio(&dormido, 36_600, 40_000, 120), Some(10.0));
        assert_eq!(consumo_medio(&puntos, 2060, 3600, 600), None);
    }

    #[test]
    fn valor_vigente_con_limite_segun_estado() {
        let descarga = [P(0, 50.0, EstadoUp::Descargando)];
        assert_eq!(valor_en(&descarga, 1000).map(|p| p.valor), Some(50.0));
        assert_eq!(valor_en(&descarga, 4000), None);
        assert_eq!(valor_en(&descarga, -1), None);
        let llena = [P(0, 80.0, EstadoUp::Estable)];
        assert_eq!(valor_en(&llena, 20_000).map(|p| p.valor), Some(80.0));
    }

    #[test]
    fn media_por_perfil_se_parece_a_lo_medido() {
        let mut a = Acumulado::default();
        a.sumar(10.0, 60.0);
        a.sumar(20.0, 60.0);
        assert!((a.vatios().expect("hay tiempo medido") - 15.0).abs() < 0.01);
        assert_eq!(a.segundos_total, 120.0);
    }

    #[test]
    fn historial_marca_tramos_de_carga() {
        let u = Upower {
            potencia: vec![
                P(0, 9.0, EstadoUp::Descargando),
                P(1800, 30.0, EstadoUp::Cargando),
            ],
            carga: vec![
                P(0, 60.0, EstadoUp::Descargando),
                P(1800, 55.0, EstadoUp::Cargando),
            ],
            modificado: None,
        };
        let h = construir_historial(&u, 0, 3600);
        assert_eq!(h.carga.len(), BARRAS_HISTORIAL);
        assert_eq!(h.carga.first(), Some(&Some(60.0)));
        assert_eq!(h.carga.last(), Some(&Some(55.0)));
        assert!(!h.cargando[0] && h.cargando[BARRAS_HISTORIAL - 1]);
    }
}

#[cfg(test)]
mod equipo_real {
    /// `cargo test lee_la_energia -- --ignored --nocapture`, como en hardware.rs.
    #[test]
    #[ignore]
    fn lee_la_energia_de_este_equipo() {
        let mut energia = super::Energia::new();
        let bateria = crate::bateria::leer();
        let estado = energia.muestra(bateria.as_ref());
        println!(
            "{}",
            serde_json::to_string_pretty(&estado).expect("el estado se serializa")
        );
        match energia.historial(24) {
            Some(h) => println!(
                "historial 24 h: {} barras, {} con carga, {} con potencia, {} cargando",
                h.carga.len(),
                h.carga.iter().flatten().count(),
                h.potencia.iter().flatten().count(),
                h.cargando.iter().filter(|c| **c).count()
            ),
            None => println!("sin historial de UPower"),
        }
    }
}
