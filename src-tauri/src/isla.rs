//! El rendimiento rápido en la isla de actividades de BookOS: CPU, GPU, RAM,
//! temperatura y batería, sin abrir la ventana.
//!
//! El resto de la app no tiene hilo de muestreo a propósito (minimizada no lee
//! nada). Esto sí lo necesita: la isla existe para verlo con la ventana
//! cerrada. Se queda en un pinchazo cada 2 s a /proc, y solo si hay escritorio
//! BookOS al otro lado del bus.

use std::time::Duration;

use tauri::AppHandle;

use crate::{bateria, monitor, sistema, Monitor};

const APP_ID: &str = "com.bookos.systemmonitor";
/// Más rápido mueve la barra sin que se lea; más lento, y un pico de CPU se
/// pierde entre dos muestras.
const PERIODO: Duration = Duration::from_secs(2);

const DESTINO: &str = "org.bookos.Desktop";
const RUTA: &str = "/org/bookos/Desktop";

fn proxy() -> zbus::Result<zbus::blocking::Proxy<'static>> {
    let c = zbus::blocking::Connection::session()?;
    zbus::blocking::Proxy::new(&c, DESTINO, RUTA, DESTINO)
}

fn fila(etiqueta: &str, fraccion: f32, texto: String, critica: bool) -> serde_json::Value {
    serde_json::json!({ "etiqueta": etiqueta, "valor": fraccion, "texto": texto, "critica": critica })
}

fn metricas(m: &mut Monitor) -> Vec<serde_json::Value> {
    let cpu = m.sistema.cpu();
    let gpu = m.gpu.leer().into_iter().find_map(|g| g.uso);
    let ram = sistema::ram();
    let bateria = bateria::leer();

    let mut filas = vec![fila("CPU", cpu.uso / 100.0, format!("{:.0} %", cpu.uso), cpu.uso >= 90.0)];
    if let Some(uso) = gpu {
        filas.push(fila("GPU", uso / 100.0, format!("{uso:.0} %"), uso >= 90.0));
    }
    let ram_uso = if ram.total > 0 { ram.usada as f32 / ram.total as f32 } else { 0.0 };
    filas.push(fila("RAM", ram_uso, format!("{:.0} %", ram_uso * 100.0), ram_uso >= 0.9));
    if let Some(t) = cpu.temperatura {
        filas.push(fila("Temperatura", t / 100.0, format!("{t:.0}°"), t >= 85.0));
    }
    if let Some(b) = bateria {
        // Baja y sin cargador es lo grave; una batería llena no es una alarma.
        filas.push(fila(
            "Batería",
            b.porcentaje / 100.0,
            format!("{:.0} %", b.porcentaje),
            b.porcentaje <= 15.0 && !b.conectado,
        ));
    }
    filas
}

pub fn publicar_en_bucle(app: AppHandle) {
    std::thread::spawn(move || {
        let Ok(bus) = zbus::blocking::Connection::session() else { return };
        let Ok(p) = zbus::blocking::Proxy::new(&bus, DESTINO, RUTA, DESTINO) else { return };
        let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&bus) else { return };
        let nombre = zbus::names::BusName::try_from(DESTINO).expect("nombre de bus constante");
        loop {
            std::thread::sleep(PERIODO);
            // Fuera de una sesión BookOS no hay isla: ni se muestrea.
            if !dbus.name_has_owner(nombre.clone()).unwrap_or(false) {
                continue;
            }
            let filas = {
                let mut m = monitor(&app);
                metricas(&mut m)
            };
            let estado = serde_json::json!({ "activo": true, "titulo": "Rendimiento", "metricas": filas });
            let _ = p.call_method("PublishActivity", &(APP_ID, "monitor", estado.to_string()));
        }
    });
}

/// Al salir, la isla no debe quedarse con una lectura congelada.
pub fn cerrar() {
    if let Ok(p) = proxy() {
        let _ = p.call_method("CloseActivity", &(APP_ID,));
    }
}
