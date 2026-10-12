#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bateria;
mod energia;
mod gpu;
mod hardware;
mod isla;
mod procesos;
mod sensores;
mod sistema;

use std::sync::{Mutex, MutexGuard};

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Los porcentajes salen de restar dos lecturas de contadores, así que todo
/// lo que guarda la lectura anterior vive aquí. No hay hilo de muestreo: el
/// frontend pide cada muestra y deja de pedir cuando la ventana no se ve, de
/// modo que la app minimizada no lee nada.
struct Monitor {
    sistema: sistema::Sistema,
    gpu: gpu::Gpus,
    procesos: procesos::Procesos,
    energia: energia::Energia,
}

fn monitor(app: &AppHandle) -> MutexGuard<'_, Monitor> {
    app.state::<Mutex<Monitor>>()
        .inner()
        .lock()
        .expect("ningún comando entra en pánico con el candado cogido")
}

#[derive(Serialize)]
struct Resumen {
    cpu: sistema::Cpu,
    gpus: Vec<gpu::Gpu>,
    ram: sistema::Ram,
    disco: sistema::Disco,
    red: sistema::Red,
    bateria: Option<bateria::Bateria>,
    energia: energia::EstadoEnergia,
    sensores: sensores::Sensores,
}

// Los comandos van con `async` para que Tauri los ejecute fuera del hilo
// principal: recorrer /proc tarda decenas de milisegundos y en el hilo
// principal congelaría la ventana mientras tanto.
#[tauri::command(async)]
fn resumen(app: AppHandle) -> Resumen {
    let mut m = monitor(&app);
    // La energía se muestrea en cada resumen, esté abierta la sección que
    // esté: así aprende el consumo de cada perfil mientras se usa el monitor.
    let bateria = bateria::leer();
    let energia = m.energia.muestra(bateria.as_ref());
    Resumen {
        cpu: m.sistema.cpu(),
        gpus: m.gpu.leer(),
        ram: sistema::ram(),
        disco: m.sistema.disco(),
        red: m.sistema.red(),
        bateria,
        energia,
        sensores: sensores::leer(),
    }
}

#[tauri::command(async)]
fn procesos(app: AppHandle, seccion: procesos::Seccion) -> Vec<procesos::Grupo> {
    let mut m = monitor(&app);
    let Monitor { procesos, gpu, .. } = &mut *m;
    procesos.leer(seccion, gpu)
}

#[tauri::command(async)]
fn hilos(app: AppHandle, pid: u32) -> Vec<procesos::Hilo> {
    monitor(&app).procesos.hilos(pid)
}

#[tauri::command(async)]
fn finalizar(app: AppHandle, pid: u32, forzar: bool) -> Result<(), String> {
    monitor(&app).procesos.finalizar(pid, forzar)
}

#[tauri::command(async)]
fn hardware() -> hardware::Hardware {
    hardware::leer()
}

#[tauri::command(async)]
fn cambiar_perfil(app: AppHandle, perfil: String) -> Result<(), String> {
    // Sin el candado: si polkit tuviera que preguntar, la ventana seguiría
    // actualizándose mientras tanto.
    energia::cambiar_perfil(&perfil)?;
    monitor(&app).energia.invalidar_perfiles();
    Ok(())
}

#[tauri::command(async)]
fn historial_bateria(app: AppHandle, horas: u32) -> Option<energia::Historial> {
    monitor(&app).energia.historial(horas)
}

/// El tema del escritorio, preguntado al portal XDG. Copiado de bookos-clock:
/// WebKitGTK no siempre sigue el tema de la sesión en `prefers-color-scheme`.
///
/// `color-scheme`: 1 oscuro, 2 claro, 0 sin preferencia.
#[tauri::command(async)]
fn tema_sistema() -> Option<String> {
    let salida = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.ReadOne",
            "org.freedesktop.appearance",
            "color-scheme",
        ])
        .output()
        .ok()?;
    // Contesta «(<uint32 1>,)».
    let texto = String::from_utf8_lossy(&salida.stdout);
    match texto.split("uint32").nth(1)?.trim_start().chars().next()? {
        '1' => Some("dark".to_string()),
        '2' => Some("light".to_string()),
        _ => None,
    }
}

/// Idioma de la sesión según el orden de prioridad de POSIX. No se usa
/// `navigator.language`: WebKitGTK lo saca de su propia lista de idiomas y no
/// siempre coincide con el locale que eligió el usuario.
#[tauri::command]
fn idioma_sistema() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_default()
}

// ── Color dinámico ───────────────────────────────────────────────────────
// La paleta la genera BookOS Settings desde el fondo de pantalla y la deja en
// ~/.config/bookos/palette.css. Aquí solo se lee: quien tiñe es la hoja, que
// el cliente bookos-palette.js inyecta al final de <head>.
#[tauri::command]
fn bookos_palette_css() -> String {
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::Path::new(&h).join(".config/bookos/palette.css"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Quita el zoom de página que WebKitGTK hace con el pellizco del touchpad.
///
/// No hay ajuste para eso ni en WebKit ni en Tauri, y la página no se entera:
/// el pellizco lo consume un `GtkGestureZoom` de la propia vista y llega como
/// `setMagnification`, no como `wheel` ni `touch*`, así que ningún
/// `preventDefault` lo frena. Se apaga ese gesto y ningún otro: con fase `None`
/// GTK deja de pasarle eventos (gtkeventcontroller.c, 3.24). Va en
/// `on_page_load` para cubrir también las recargas. Igual que en bookos-clock.
fn desactivar_zoom_por_pellizco<R: tauri::Runtime>(
    webview: &tauri::Webview<R>,
    _: &tauri::webview::PageLoadPayload<'_>,
) {
    #[cfg(target_os = "linux")]
    let _ = webview.with_webview(|webview| {
        use gtk::glib::translate::from_glib_none;
        use gtk::prelude::*;

        // WebKitGTK guarda ahí el gesto (WebKitWebViewBase.cpp, 2.52.5). No es
        // API pública: si la clave cambia, no se encuentra y el zoom vuelve.
        let Some(gesto) = (unsafe {
            webview
                .inner()
                .data::<gtk::ffi::GtkGesture>("wk-view-zoom-gesture")
        }) else {
            return;
        };
        let gesto: gtk::Gesture = unsafe { from_glib_none(gesto.as_ptr()) };
        gesto.set_propagation_phase(gtk::PropagationPhase::None);
    });
}

#[tauri::command]
fn bookos_language() -> String {
    let locale = std::env::var("LANG").unwrap_or_else(|_| "en".into());
    let Some(home) = std::env::var_os("HOME") else {
        return if locale.to_lowercase().starts_with("es") { "es" } else { "en" }.into();
    };
    let path = std::path::PathBuf::from(home).join(".config/bookos/settings.json");
    let language = std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|settings| settings.get("AppLanguage").and_then(|value| value.as_str()).map(str::to_owned));
    match language.as_deref() {
        Some("es") => "es".into(),
        Some("en") => "en".into(),
        _ if locale.to_lowercase().starts_with("es") => "es".into(),
        _ => "en".into(),
    }
}

fn main() {
    tauri::Builder::default()
        .on_page_load(desactivar_zoom_por_pellizco)
        .manage(Mutex::new(Monitor {
            sistema: sistema::Sistema::new(),
            gpu: gpu::Gpus::new(),
            procesos: procesos::Procesos::new(),
            energia: energia::Energia::new(),
        }))
        .setup(|app| {
            // La ventana nace invisible para no verla a medio dibujar: la
            // enseña el frontend cuando ya tiene la primera muestra. Si el
            // JavaScript falla antes de llegar ahí, no la enseñaría nadie y la
            // app quedaría corriendo sin ventana, así que se enseña igual.
            let ventana = app
                .get_webview_window("main")
                .expect("la ventana «main» está declarada en tauri.conf.json");
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                if ventana.is_visible().is_ok_and(|visible| !visible) {
                    let _ = ventana.show();
                }
            });
            isla::publicar_en_bucle(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            resumen,
            procesos,
            hilos,
            finalizar,
            hardware,
            cambiar_perfil,
            historial_bateria,
            tema_sistema,
            bookos_language,
            idioma_sistema,
            bookos_palette_css
        ])
        .build(tauri::generate_context!())
        .expect("error al arrancar la aplicación")
        .run(|_, evento| {
            if matches!(evento, tauri::RunEvent::Exit) {
                isla::cerrar();
            }
        });
}
