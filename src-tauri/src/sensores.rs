//! Sensores del equipo: temperaturas, ventiladores, tensiones y corrientes.
//!
//! Todo sale de hwmon (/sys/class/hwmon), que es donde el kernel deja lo que
//! publican los chips, y de las zonas térmicas para lo que no tiene hwmon
//! propio. Sin lm_sensors ni permisos: los mismos ficheros que lee `sensors`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Un sensor puede publicar un umbral disparatado cuando en realidad no tiene:
/// el NVMe de este equipo da 65 261 °C de máximo en dos de sus tres sensores.
const TEMPERATURA_ABSURDA: f32 = 200.0;

#[derive(Serialize, Debug, PartialEq)]
pub struct Lectura {
    etiqueta: String,
    valor: f32,
    maximo: Option<f32>,
    critico: Option<f32>,
}

#[derive(Serialize)]
pub struct Chip {
    /// Nombre que da el kernel: coretemp, nvme, iwlwifi_1, acpitz…
    nombre: String,
    temperaturas: Vec<Lectura>,
    ventiladores: Vec<Lectura>,
    tensiones: Vec<Lectura>,
    corrientes: Vec<Lectura>,
    potencias: Vec<Lectura>,
}

#[derive(Serialize)]
pub struct Ventilador {
    nombre: String,
    /// Vueltas por minuto, solo si el firmware las da.
    rpm: Option<f32>,
    /// Nivel de refrigeración del kernel; en muchos portátiles es 0 o 1.
    nivel: Option<u32>,
    nivel_maximo: Option<u32>,
}

/// Modo de rendimiento del firmware. Suele tener más modos que los tres de
/// power-profiles-daemon: este Galaxy Book añade «quiet».
#[derive(Serialize)]
pub struct Plataforma {
    modo: String,
    opciones: Vec<String>,
    controlador: Option<String>,
}

/// Interruptor que publica el driver del fabricante (carga por USB con el
/// equipo apagado, encender al abrir la tapa…).
#[derive(Serialize)]
pub struct Ajuste {
    nombre: String,
    valor: String,
    opciones: Vec<String>,
}

#[derive(Serialize)]
pub struct Sensores {
    chips: Vec<Chip>,
    ventilacion: Vec<Ventilador>,
    plataforma: Option<Plataforma>,
    firmware: Vec<Ajuste>,
}

impl Chip {
    fn vacio(&self) -> bool {
        self.temperaturas.is_empty()
            && self.ventiladores.is_empty()
            && self.tensiones.is_empty()
            && self.corrientes.is_empty()
            && self.potencias.is_empty()
    }
}

pub fn leer() -> Sensores {
    let mut chips: Vec<Chip> = Vec::new();
    let mut con_hwmon: HashSet<PathBuf> = HashSet::new();
    for ruta in directorios("/sys/class/hwmon") {
        let Some(nombre) = fs::read_to_string(ruta.join("name"))
            .ok()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        // Un chip puede colgar de una zona térmica: si está aquí, no se repite
        // luego como zona suelta.
        if let Ok(destino) = fs::canonicalize(ruta.join("device")) {
            con_hwmon.insert(destino);
        }
        let chip = Chip {
            nombre,
            // m°C, rpm, mV, mA y µW: así los publica hwmon.
            temperaturas: lecturas(&ruta, "temp", 1000.0, TEMPERATURA_ABSURDA),
            ventiladores: lecturas(&ruta, "fan", 1.0, 100_000.0),
            tensiones: lecturas(&ruta, "in", 1000.0, 1000.0),
            corrientes: lecturas(&ruta, "curr", 1000.0, 1000.0),
            potencias: lecturas(&ruta, "power", 1_000_000.0, 10_000.0),
        };
        if !chip.vacio() {
            chips.push(chip);
        }
    }
    chips.sort_by(|a, b| a.nombre.cmp(&b.nombre));
    if let Some(zonas) = zonas_termicas(&con_hwmon) {
        chips.push(zonas);
    }
    Sensores {
        chips,
        ventilacion: ventilacion(),
        plataforma: plataforma(),
        firmware: firmware(),
    }
}

/// Ventiladores vistos por el kernel. Las vueltas por minuto solo salen si el
/// firmware contesta: en este Galaxy Book el driver publica `fan_speed_rpm` y
/// el hwmon `fan1_input`, pero los dos dan error de E/S, así que queda el
/// nivel de refrigeración (encendido o apagado).
fn ventilacion() -> Vec<Ventilador> {
    directorios("/sys/class/thermal")
        .into_iter()
        .filter(|r| nombre_empieza(r, "cooling_device"))
        .filter_map(|dispositivo| {
            let tipo = fs::read_to_string(dispositivo.join("type")).ok()?.trim().to_string();
            if !tipo.to_lowercase().contains("fan") {
                return None;
            }
            let acpi = fs::canonicalize(dispositivo.join("device")).ok();
            Some(Ventilador {
                nombre: tipo,
                rpm: acpi
                    .and_then(|d| numero(&d.join("fan_speed_rpm")))
                    .filter(|r| *r > 0.0),
                nivel: numero(&dispositivo.join("cur_state")).filter(|n| *n >= 0.0).map(|n| n as u32),
                nivel_maximo: numero(&dispositivo.join("max_state")).filter(|n| *n > 0.0).map(|n| n as u32),
            })
        })
        .collect()
}

fn plataforma() -> Option<Plataforma> {
    let clase = directorios("/sys/class/platform-profile").into_iter().next();
    let (modo, opciones, controlador) = match &clase {
        Some(dir) => (
            fs::read_to_string(dir.join("profile")).ok()?,
            fs::read_to_string(dir.join("choices")).ok()?,
            fs::read_to_string(dir.join("name")).ok().map(|n| n.trim().to_string()),
        ),
        // Kernels anteriores a la clase platform-profile.
        None => (
            fs::read_to_string("/sys/firmware/acpi/platform_profile").ok()?,
            fs::read_to_string("/sys/firmware/acpi/platform_profile_choices").ok()?,
            None,
        ),
    };
    Some(Plataforma {
        modo: modo.trim().to_string(),
        opciones: opciones.split_whitespace().map(str::to_string).collect(),
        controlador,
    })
}

/// Solo de lectura: cambiar estos interruptores pide permisos de administrador.
fn firmware() -> Vec<Ajuste> {
    directorios("/sys/class/firmware-attributes")
        .into_iter()
        .flat_map(|driver| directorios(driver.join("attributes")))
        .filter_map(|atributo| {
            let valor = fs::read_to_string(atributo.join("current_value")).ok()?.trim().to_string();
            let nombre = fs::read_to_string(atributo.join("display_name"))
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .or_else(|| atributo.file_name()?.to_str().map(str::to_string))?;
            Some(Ajuste {
                nombre,
                valor,
                opciones: fs::read_to_string(atributo.join("possible_values"))
                    .ok()
                    .map(|v| v.trim().split(';').map(str::to_string).collect())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn nombre_empieza(ruta: &Path, prefijo: &str) -> bool {
    ruta.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(prefijo))
}

fn directorios(base: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut rutas: Vec<PathBuf> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    rutas.sort();
    rutas
}

fn numero(ruta: &Path) -> Option<f32> {
    fs::read_to_string(ruta).ok()?.trim().parse().ok()
}

/// Índice de «temp10_input» con prefijo «temp» → 10. Descarta cosas como
/// «intrusion0_alarm» o «temp1_crit», que no son lecturas.
fn indice(nombre: &str, prefijo: &str) -> Option<u32> {
    nombre.strip_prefix(prefijo)?.strip_suffix("_input")?.parse().ok()
}

/// Un umbral solo vale si es un número positivo y creíble para la magnitud.
fn umbral(valor: Option<f32>, tope: f32) -> Option<f32> {
    valor.filter(|v| *v > 0.0 && *v < tope)
}

fn lecturas(chip: &Path, prefijo: &str, escala: f32, tope: f32) -> Vec<Lectura> {
    let mut lecturas: Vec<(u32, Lectura)> = fs::read_dir(chip)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let nombre = e.file_name().into_string().ok()?;
            let n = indice(&nombre, prefijo)?;
            let valor = numero(&e.path())? / escala;
            let etiqueta = fs::read_to_string(chip.join(format!("{prefijo}{n}_label")))
                .ok()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| format!("{prefijo}{n}"));
            Some((
                n,
                Lectura {
                    etiqueta,
                    valor,
                    maximo: umbral(numero(&chip.join(format!("{prefijo}{n}_max"))).map(|v| v / escala), tope),
                    critico: umbral(numero(&chip.join(format!("{prefijo}{n}_crit"))).map(|v| v / escala), tope),
                },
            ))
        })
        .collect();
    lecturas.sort_by_key(|(n, _)| *n);
    lecturas.into_iter().map(|(_, l)| l).collect()
}

/// Sensores que solo existen como zona térmica (SNS1, TCPU, x86_pkg_temp…).
fn zonas_termicas(con_hwmon: &HashSet<PathBuf>) -> Option<Chip> {
    let lecturas: Vec<Lectura> = directorios("/sys/class/thermal")
        .into_iter()
        .filter(|r| nombre_empieza(r, "thermal_zone"))
        .filter(|r| !fs::canonicalize(r).is_ok_and(|d| con_hwmon.contains(&d)))
        .filter_map(|zona| {
            Some(Lectura {
                etiqueta: fs::read_to_string(zona.join("type")).ok()?.trim().to_string(),
                valor: numero(&zona.join("temp"))? / 1000.0,
                maximo: None,
                critico: umbral(critico_de_zona(&zona), TEMPERATURA_ABSURDA),
            })
        })
        .collect();
    (!lecturas.is_empty()).then(|| Chip {
        nombre: "thermal".to_string(),
        temperaturas: lecturas,
        ventiladores: Vec::new(),
        tensiones: Vec::new(),
        corrientes: Vec::new(),
        potencias: Vec::new(),
    })
}

/// El punto de disparo «critical» de una zona térmica, si lo declara.
fn critico_de_zona(zona: &Path) -> Option<f32> {
    (0..16).find_map(|i| {
        let tipo = fs::read_to_string(zona.join(format!("trip_point_{i}_type"))).ok()?;
        (tipo.trim() == "critical").then(|| numero(&zona.join(format!("trip_point_{i}_temp"))))?
    })
    .map(|t| t / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_de_ficheros_hwmon() {
        assert_eq!(indice("temp10_input", "temp"), Some(10));
        assert_eq!(indice("temp1_crit", "temp"), None);
        assert_eq!(indice("intrusion0_alarm", "in"), None);
        assert_eq!(indice("in0_input", "in"), Some(0));
    }

    #[test]
    fn umbrales_imposibles_se_descartan() {
        // El NVMe de este equipo publica 65 261 850 m°C como máximo.
        assert_eq!(umbral(Some(65261.85), TEMPERATURA_ABSURDA), None);
        assert_eq!(umbral(Some(84.85), TEMPERATURA_ABSURDA), Some(84.85));
        // Los puertos USB-C dan 0 cuando no hay nada enchufado.
        assert_eq!(umbral(Some(0.0), 1000.0), None);
    }
}
