//! Procesos agrupados como los ve el usuario: aplicaciones, sesión, segundo
//! plano y sistema.
//!
//! La agrupación sale del cgroup de cada proceso. Plasma, GNOME y los lanzadores
//! que siguen la especificación XDG de systemd meten cada app abierta en su
//! propia unidad `app-<id>@….service` o `app-<id>-<n>.scope` dentro de
//! `app.slice`, y todos sus hijos heredan la unidad: una pestaña de konsole y lo
//! que se lance en ella caen en el grupo de konsole. Con eso no hace falta
//! preguntar al compositor qué ventanas hay, que en Wayland no se puede.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

use crate::gpu::Gpus;

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Seccion {
    Cpu,
    Gpu,
    Ram,
    Disco,
    Red,
    Bateria,
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Clase {
    Aplicacion,
    Sesion,
    SegundoPlano,
    Sistema,
}

#[derive(Serialize)]
pub struct Proceso {
    pid: u32,
    padre: Option<u32>,
    nombre: String,
    cpu: f32,
    memoria: u64,
    hilos: usize,
    lectura: u64,
    escritura: u64,
    gpu: f32,
    memoria_gpu: u64,
    /// Del mismo usuario que ejecuta el monitor.
    propio: bool,
}

#[derive(Serialize)]
pub struct Grupo {
    id: String,
    nombre: String,
    icono: Option<String>,
    clase: Clase,
    procesos: Vec<Proceso>,
}

#[derive(Serialize)]
pub struct Hilo {
    tid: u32,
    nombre: String,
    cpu: f32,
}

#[derive(Clone)]
struct Meta {
    id: String,
    nombre: String,
    icono: Option<String>,
    clase: Clase,
}

/// Unidad de systemd tal como sale del cgroup, antes de buscar su .desktop.
#[derive(Debug, PartialEq)]
struct Unidad {
    /// Nombre completo de la unidad: distingue instancias de una misma app.
    clave: String,
    clase: Clase,
    /// Id de aplicación XDG o, si no lo hay, el nombre legible de la unidad.
    id: String,
    lanzada_por_usuario: bool,
}

#[derive(Default)]
struct Catalogo {
    metas: Vec<Meta>,
    por_unidad: HashMap<String, usize>,
    por_id: HashMap<String, usize>,
    /// pid → (arranque, índice en `metas`). El arranque detecta un PID reciclado.
    por_pid: HashMap<u32, (u64, usize)>,
    tema_iconos: Option<String>,
    idioma: String,
}

pub struct Procesos {
    sys: System,
    catalogo: Catalogo,
    ultima_lectura_disco: Option<Instant>,
}

impl Procesos {
    pub fn new() -> Self {
        let idioma = std::env::var("LANG")
            .unwrap_or_default()
            .split(['_', '.'])
            .next()
            .unwrap_or_default()
            .to_string();
        Procesos {
            sys: System::new(),
            catalogo: Catalogo {
                tema_iconos: tema_iconos(),
                idioma,
                ..Default::default()
            },
            ultima_lectura_disco: None,
        }
    }

    pub fn leer(&mut self, seccion: Seccion, gpus: &mut Gpus) -> Vec<Grupo> {
        // La línea de órdenes y el usuario solo se leen una vez por proceso:
        // hacen falta para el nombre y el filtro «mis procesos», y no cambian.
        let mut tipo = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet);
        let mut segundos_disco = 0.0;
        if seccion == Seccion::Disco {
            tipo = tipo.with_disk_usage();
            let ahora = Instant::now();
            if let Some(antes) = self.ultima_lectura_disco.replace(ahora) {
                segundos_disco = ahora.duration_since(antes).as_secs_f64();
            }
        }
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, tipo);

        // sysinfo mete cada hilo en el mapa como si fuera un proceso, y los
        // hilos del núcleo también. Ninguno de los dos es una fila.
        let procesos: Vec<&Process> = self
            .sys
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .collect();

        let uso_gpu = if seccion == Seccion::Gpu {
            let pids: Vec<u32> = procesos.iter().map(|p| p.pid().as_u32()).collect();
            gpus.por_proceso(&pids)
        } else {
            HashMap::new()
        };

        // El monitor está en la lista que se acaba de leer: su usuario es el propio.
        let uid_propio = sysinfo::get_current_pid()
            .ok()
            .and_then(|pid| self.sys.process(pid))
            .and_then(Process::user_id)
            .cloned();

        let catalogo = &mut self.catalogo;
        catalogo
            .por_pid
            .retain(|pid, _| self.sys.process(Pid::from_u32(*pid)).is_some());

        let mut grupos: HashMap<usize, Vec<Proceso>> = HashMap::new();
        for p in procesos {
            let pid = p.pid().as_u32();
            let indice = catalogo.indice(pid, p.start_time());
            let disco = p.disk_usage();
            let por_segundo = |bytes: u64| {
                if segundos_disco > 0.0 {
                    (bytes as f64 / segundos_disco) as u64
                } else {
                    0
                }
            };
            let gpu = uso_gpu.get(&pid).copied().unwrap_or_default();
            grupos.entry(indice).or_default().push(Proceso {
                pid,
                padre: p.parent().map(Pid::as_u32),
                nombre: nombre_proceso(p),
                cpu: p.cpu_usage(),
                memoria: p.memory(),
                // El hilo principal no está en `tasks`: su TID es el propio PID.
                hilos: p.tasks().map_or(0, |t| t.len()) + 1,
                lectura: por_segundo(disco.read_bytes),
                escritura: por_segundo(disco.written_bytes),
                gpu: gpu.uso,
                memoria_gpu: gpu.memoria,
                propio: uid_propio.is_some() && p.user_id() == uid_propio.as_ref(),
            });
        }

        grupos
            .into_iter()
            .map(|(indice, procesos)| {
                let meta = &catalogo.metas[indice];
                Grupo {
                    id: meta.id.clone(),
                    nombre: meta.nombre.clone(),
                    icono: meta.icono.clone(),
                    clase: meta.clase,
                    procesos,
                }
            })
            .collect()
    }

    pub fn hilos(&self, pid: u32) -> Vec<Hilo> {
        let Some(proceso) = self.sys.process(Pid::from_u32(pid)) else {
            return Vec::new();
        };
        let mut hilos: Vec<Hilo> = proceso
            .tasks()
            .into_iter()
            .flatten()
            .filter_map(|tid| self.sys.process(*tid))
            .map(|t| Hilo {
                tid: t.pid().as_u32(),
                nombre: t.name().to_string_lossy().into_owned(),
                cpu: t.cpu_usage(),
            })
            .collect();
        hilos.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(a.tid.cmp(&b.tid)));
        hilos
    }

    /// Devuelve la clave de traducción del error para que el frontend la muestre.
    pub fn finalizar(&self, pid: u32, forzar: bool) -> Result<(), String> {
        let proceso = self
            .sys
            .process(Pid::from_u32(pid))
            .ok_or("error.no_existe")?;
        let senal = if forzar { Signal::Kill } else { Signal::Term };
        // sysinfo no devuelve el errno; con un PID vivo, lo único que hace
        // fallar kill() es no tener permiso sobre un proceso ajeno.
        match proceso.kill_with(senal) {
            Some(true) => Ok(()),
            _ => Err("error.sin_permiso".into()),
        }
    }
}

impl Catalogo {
    fn indice(&mut self, pid: u32, arranque: u64) -> usize {
        if let Some(&(a, i)) = self.por_pid.get(&pid) {
            if a == arranque {
                return i;
            }
        }
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        let unidad = clasificar(&cgroup);
        let i = match self.por_unidad.get(&unidad.clave) {
            Some(&i) => i,
            None => {
                let meta = self.meta(&unidad);
                let i = match self.por_id.get(&meta.id) {
                    Some(&i) => i,
                    None => {
                        self.por_id.insert(meta.id.clone(), self.metas.len());
                        self.metas.push(meta);
                        self.metas.len() - 1
                    }
                };
                self.por_unidad.insert(unidad.clave, i);
                i
            }
        };
        self.por_pid.insert(pid, (arranque, i));
        i
    }

    fn meta(&self, unidad: &Unidad) -> Meta {
        let escritorio = buscar_desktop(&unidad.id).and_then(|ruta| {
            let texto = fs::read_to_string(&ruta).ok()?;
            let stem = ruta.file_stem()?.to_str()?.to_string();
            Some((stem, leer_desktop(&texto, &self.idioma)))
        });
        let clase = match (&escritorio, unidad.clase) {
            (Some((_, d)), Clase::Aplicacion) if unidad.lanzada_por_usuario && !d.oculta => {
                Clase::Aplicacion
            }
            (_, Clase::Aplicacion) => Clase::SegundoPlano,
            (_, otra) => otra,
        };
        match escritorio {
            // Dos instancias de Dolphin, una lanzada como «dolphin» y otra como
            // «org.kde.dolphin», acaban en el mismo .desktop y en el mismo grupo.
            Some((stem, d)) => Meta {
                id: format!("{clase:?}:{stem}"),
                nombre: d.nombre.unwrap_or_else(|| unidad.id.clone()),
                icono: d
                    .icono
                    .and_then(|i| resolver_icono(&i, self.tema_iconos.as_deref())),
                clase,
            },
            None => Meta {
                id: format!("{clase:?}:{}", unidad.id),
                nombre: unidad.id.clone(),
                icono: None,
                clase,
            },
        }
    }
}

fn clasificar(cgroup: &str) -> Unidad {
    let ruta = cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("/");
    let partes: Vec<&str> = ruta.split('/').filter(|p| !p.is_empty()).collect();
    let tras = |slice: &str| {
        let i = partes.iter().position(|p| *p == slice)?;
        partes.get(i + 1).copied()
    };

    if let Some(u) = tras("app.slice") {
        if let Some(id) = id_aplicacion(u) {
            return Unidad {
                clave: u.to_string(),
                clase: Clase::Aplicacion,
                id,
                // Lo que arranca solo al iniciar sesión vive en app.slice pero
                // son demonios (kdeconnect, el puente de Xwayland).
                lanzada_por_usuario: !u.contains("@autostart."),
            };
        }
        return unidad_simple(u, Clase::SegundoPlano);
    }
    if let Some(u) = tras("session.slice") {
        return unidad_simple(u, Clase::Sesion);
    }
    if let Some(u) = tras("background.slice") {
        return unidad_simple(u, Clase::SegundoPlano);
    }
    if let Some(u) = partes
        .iter()
        .find(|p| p.starts_with("session-") && p.ends_with(".scope"))
    {
        return unidad_simple(u, Clase::Sesion);
    }
    match partes.last() {
        Some(u) => unidad_simple(u, Clase::Sistema),
        None => unidad_simple("init.scope", Clase::Sistema),
    }
}

fn unidad_simple(unidad: &str, clase: Clase) -> Unidad {
    let sin_tipo = unidad
        .trim_end_matches(".service")
        .trim_end_matches(".scope")
        .trim_end_matches(".slice");
    let base = sin_tipo.split('@').next().unwrap_or(sin_tipo);
    Unidad {
        clave: unidad.to_string(),
        clase,
        id: desescapar(base.strip_prefix("plasma-").unwrap_or(base)),
        lanzada_por_usuario: false,
    }
}

/// `app[-<lanzador>]-<id>[@<aleatorio>].service` o
/// `app[-<lanzador>]-<id>-<aleatorio>.scope`. Los guiones del id vienen
/// escapados como `\x2d`, así que un guion literal siempre separa campos.
fn id_aplicacion(unidad: &str) -> Option<String> {
    let resto = unidad.strip_prefix("app-")?;
    let resto = if let Some(s) = resto.strip_suffix(".service") {
        s.split('@').next()?
    } else {
        resto.strip_suffix(".scope")?.rsplit_once('-')?.0
    };
    let id = resto.rsplit('-').next()?;
    (!id.is_empty()).then(|| desescapar(id))
}

fn desescapar(texto: &str) -> String {
    let mut salida = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(i) = resto.find("\\x") {
        salida.push_str(&resto[..i]);
        let byte = resto
            .get(i + 2..i + 4)
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match byte {
            Some(b) => {
                salida.push(char::from(b));
                resto = &resto[i + 4..];
            }
            None => {
                salida.push_str("\\x");
                resto = &resto[i + 2..];
            }
        }
    }
    salida.push_str(resto);
    salida
}

fn nombre_proceso(p: &Process) -> String {
    let nombre = p.name().to_string_lossy();
    // El kernel corta comm a 15 bytes. Si el ejecutable empieza igual, es el
    // nombre entero; si no («Isolated Web Co» de firefox), comm dice más.
    if nombre.len() >= 15 {
        let base = p
            .cmd()
            .first()
            .and_then(|c| Path::new(c).file_name())
            .and_then(|b| b.to_str());
        if let Some(base) = base.filter(|b| b.starts_with(&*nombre)) {
            return base.to_string();
        }
    }
    nombre.into_owned()
}

fn directorios_datos() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")))
        .into_iter()
        .collect();
    if let Some(h) = &home {
        dirs.push(h.join(".local/share/flatpak/exports/share"));
    }
    dirs.extend(
        [
            "/var/lib/flatpak/exports/share",
            "/usr/local/share",
            "/usr/share",
        ]
        .map(PathBuf::from),
    );
    dirs
}

fn buscar_desktop(id: &str) -> Option<PathBuf> {
    let aplicaciones: Vec<PathBuf> = directorios_datos()
        .into_iter()
        .map(|d| d.join("applications"))
        .collect();
    let exacto = format!("{id}.desktop");
    if let Some(ruta) = aplicaciones
        .iter()
        .map(|d| d.join(&exacto))
        .find(|r| r.is_file())
    {
        return Some(ruta);
    }
    // «dolphin» → org.kde.dolphin.desktop. Solo se llega aquí una vez por
    // unidad nueva, así que listar el directorio no se nota.
    let sufijo = format!(".{}", exacto.to_lowercase());
    aplicaciones.iter().find_map(|d| {
        fs::read_dir(d).ok()?.flatten().map(|e| e.path()).find(|r| {
            r.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_lowercase().ends_with(&sufijo))
        })
    })
}

#[derive(Debug, Default, PartialEq)]
struct Desktop {
    nombre: Option<String>,
    icono: Option<String>,
    oculta: bool,
}

fn leer_desktop(texto: &str, idioma: &str) -> Desktop {
    let mut d = Desktop::default();
    let mut nombre_local = None;
    let clave_local = format!("Name[{idioma}]");
    let mut en_entrada = false;
    for linea in texto.lines().map(str::trim) {
        if linea.starts_with('[') {
            en_entrada = linea == "[Desktop Entry]";
            continue;
        }
        let Some((clave, valor)) = linea.split_once('=').filter(|_| en_entrada) else {
            continue;
        };
        match clave.trim() {
            "Name" => d.nombre = Some(valor.trim().to_string()),
            c if c == clave_local => nombre_local = Some(valor.trim().to_string()),
            "Icon" => d.icono = Some(valor.trim().to_string()),
            "NoDisplay" | "Hidden" if valor.trim() == "true" => d.oculta = true,
            _ => {}
        }
    }
    if nombre_local.is_some() {
        d.nombre = nombre_local;
    }
    d
}

fn tema_iconos() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let texto = fs::read_to_string(Path::new(&home).join(".config/kdeglobals")).ok()?;
    let mut en_iconos = false;
    texto.lines().map(str::trim).find_map(|l| {
        if l.starts_with('[') {
            en_iconos = l == "[Icons]";
            return None;
        }
        l.strip_prefix("Theme=")
            .filter(|_| en_iconos)
            .map(|t| t.trim().to_string())
    })
}

fn resolver_icono(icono: &str, tema: Option<&str>) -> Option<String> {
    if icono.starts_with('/') {
        return Path::new(icono).is_file().then(|| icono.to_string());
    }
    let bases: Vec<PathBuf> = directorios_datos()
        .into_iter()
        .map(|d| d.join("icons"))
        .collect();
    // Los temas KDE usan apps/48; los freedesktop, 48x48/apps. Del más nítido
    // al más pequeño que aún se ve bien a 22 px con densidad 2.
    const CARPETAS: [&str; 8] = [
        "scalable/apps",
        "apps/scalable",
        "48x48/apps",
        "apps/48",
        "64x64/apps",
        "128x128/apps",
        "256x256/apps",
        "32x32/apps",
    ];
    let temas = tema.into_iter().chain(["breeze", "hicolor"]);
    for tema in temas {
        for base in &bases {
            let dir_tema = base.join(tema);
            if !dir_tema.is_dir() {
                continue;
            }
            for carpeta in CARPETAS {
                for ext in ["svg", "png"] {
                    let ruta = dir_tema.join(carpeta).join(format!("{icono}.{ext}"));
                    if ruta.is_file() {
                        return ruta.to_str().map(str::to_string);
                    }
                }
            }
        }
    }
    ["svg", "png"]
        .iter()
        .map(|ext| PathBuf::from(format!("/usr/share/pixmaps/{icono}.{ext}")))
        .find(|r| r.is_file())
        .and_then(|r| r.to_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USUARIO: &str = "0::/user.slice/user-1000.slice/user@1000.service";

    #[test]
    fn cgroup_de_firefox_es_aplicacion() {
        let u = clasificar(&format!(
            "{USUARIO}/app.slice/app-org.mozilla.firefox@7eabd4b0d5ba4d1fae80b9aea2520f96.service\n"
        ));
        assert_eq!(u.clase, Clase::Aplicacion);
        assert_eq!(u.id, "org.mozilla.firefox");
        assert!(u.lanzada_por_usuario);
    }

    #[test]
    fn pestana_de_konsole_cuenta_para_konsole() {
        let u = clasificar(&format!(
            "{USUARIO}/app.slice/app-org.kde.konsole-386105.scope/tab(402366).scope"
        ));
        assert_eq!(u.id, "org.kde.konsole");
        assert_eq!(u.clave, "app-org.kde.konsole-386105.scope");
    }

    #[test]
    fn id_con_guiones_escapados() {
        let u = clasificar(&format!(
            "{USUARIO}/app.slice/app-bookos\\x2dnotepad@45f523d37e2a45a79508b8831ebf0d60.service"
        ));
        assert_eq!(u.id, "bookos-notepad");
    }

    #[test]
    fn scope_con_lanzador_gnome() {
        assert_eq!(
            id_aplicacion("app-gnome-org.gnome.Nautilus-4242.scope").as_deref(),
            Some("org.gnome.Nautilus")
        );
    }

    #[test]
    fn autostart_no_es_lanzada_por_usuario() {
        let u = clasificar(&format!(
            "{USUARIO}/app.slice/app-org.kde.kdeconnect.daemon@autostart.service"
        ));
        assert_eq!(u.id, "org.kde.kdeconnect.daemon");
        assert!(!u.lanzada_por_usuario);
    }

    #[test]
    fn servicios_de_app_slice_sin_app_son_segundo_plano() {
        let u = clasificar(&format!("{USUARIO}/app.slice/ssh-agent.service"));
        assert_eq!(u.clase, Clase::SegundoPlano);
        assert_eq!(u.id, "ssh-agent");
    }

    #[test]
    fn sesion_segundo_plano_y_sistema() {
        let plasma = clasificar(&format!(
            "{USUARIO}/session.slice/plasma-plasmashell.service"
        ));
        assert_eq!(
            (plasma.clase, plasma.id.as_str()),
            (Clase::Sesion, "plasmashell")
        );
        let akonadi = clasificar(&format!(
            "{USUARIO}/background.slice/akonadi_control.service"
        ));
        assert_eq!(akonadi.clase, Clase::SegundoPlano);
        let login = clasificar("0::/user.slice/user-1000.slice/session-2.scope");
        assert_eq!(login.clase, Clase::Sesion);
        let nm = clasificar("0::/system.slice/NetworkManager.service");
        assert_eq!(
            (nm.clase, nm.id.as_str()),
            (Clase::Sistema, "NetworkManager")
        );
        assert_eq!(clasificar("0::/").clase, Clase::Sistema);
    }

    #[test]
    fn desktop_prefiere_nombre_localizado_e_ignora_acciones() {
        let texto = "[Desktop Entry]\nName=Files\nName[es]=Archivos\nIcon=org.kde.dolphin\n\
                     \n[Desktop Action new]\nName=Nueva ventana\nNoDisplay=true\n";
        assert_eq!(
            leer_desktop(texto, "es"),
            Desktop {
                nombre: Some("Archivos".into()),
                icono: Some("org.kde.dolphin".into()),
                oculta: false
            }
        );
        assert_eq!(leer_desktop(texto, "en").nombre.as_deref(), Some("Files"));
    }
}
