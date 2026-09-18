//! Batería leída de /sys/class/power_supply. sysinfo no la cubre.

use std::collections::HashMap;
use std::fs;

use serde::Serialize;

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Estado {
    Cargando,
    Descargando,
    Llena,
    // El cargador está puesto pero el firmware no carga: pasa con el límite
    // de carga al 80 % que traen los Galaxy Book.
    EnPausa,
    Desconocido,
}

#[derive(Serialize, Debug)]
pub struct Bateria {
    porcentaje: f32,
    estado: Estado,
    conectado: bool,
    salud: Option<f32>,
    ciclos: Option<u32>,
    potencia_w: Option<f32>,
    minutos_restantes: Option<u32>,
    limite_carga: Option<u8>,
    capacidad_wh: Option<f32>,
    capacidad_diseno_wh: Option<f32>,
    tecnologia: Option<String>,
    fabricante: Option<String>,
    modelo: Option<String>,
    serie: Option<String>,
    energia_wh: Option<f32>,
    tension_v: Option<f32>,
    tension_diseno_v: Option<f32>,
    temperatura_c: Option<f32>,
}

impl Bateria {
    pub(crate) fn descargando(&self) -> bool {
        self.estado == Estado::Descargando
    }

    pub(crate) fn potencia_w(&self) -> Option<f32> {
        self.potencia_w
    }

    pub(crate) fn energia_wh(&self) -> Option<f32> {
        self.energia_wh
    }
}

pub fn leer() -> Option<Bateria> {
    let mut bateria = None;
    let mut conectado = false;
    for entrada in fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let Ok(texto) = fs::read_to_string(entrada.path().join("uevent")) else {
            continue;
        };
        let campos = campos_uevent(&texto);
        let online = campos.get("ONLINE") == Some(&"1");
        match campos.get("TYPE").copied() {
            // SCOPE=Device son las pilas de ratones y mandos, no la del equipo.
            Some("Battery") if campos.get("SCOPE") != Some(&"Device") && bateria.is_none() => {
                bateria = desde_uevent(&campos);
            }
            // Los puertos USB-C (ucsi) informan de la carga con TYPE=USB.
            Some("Mains" | "USB") if online => conectado = true,
            _ => {}
        }
    }
    bateria.map(|mut b| {
        b.conectado = conectado || matches!(b.estado, Estado::Cargando | Estado::EnPausa);
        b
    })
}

fn campos_uevent(texto: &str) -> HashMap<&str, &str> {
    texto
        .lines()
        .filter_map(|l| l.strip_prefix("POWER_SUPPLY_")?.split_once('='))
        .collect()
}

fn desde_uevent(c: &HashMap<&str, &str>) -> Option<Bateria> {
    let num = |k: &str| c.get(k).and_then(|v| v.parse::<f64>().ok());
    let texto = |k: &str| {
        c.get(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let porcentaje = num("CAPACITY")?;
    let estado = match c.get("STATUS").copied() {
        Some("Charging") => Estado::Cargando,
        Some("Discharging") => Estado::Descargando,
        Some("Full") => Estado::Llena,
        Some("Not charging") => Estado::EnPausa,
        _ => Estado::Desconocido,
    };

    // Unos firmwares dan energía (µWh y µW) y otros carga (µAh y µA). Los
    // tiempos se calculan en las unidades nativas para no mezclar tensiones;
    // solo los Wh que se enseñan pasan por la tensión nominal.
    let por_energia = num("ENERGY_NOW").is_some();
    let (ahora, llena, diseno, ritmo) = if por_energia {
        (
            num("ENERGY_NOW"),
            num("ENERGY_FULL"),
            num("ENERGY_FULL_DESIGN"),
            num("POWER_NOW"),
        )
    } else {
        (
            num("CHARGE_NOW"),
            num("CHARGE_FULL"),
            num("CHARGE_FULL_DESIGN"),
            num("CURRENT_NOW"),
        )
    };
    let ritmo = ritmo.map(f64::abs).filter(|r| *r > 0.0);
    let limite = num("CHARGE_CONTROL_END_THRESHOLD")
        .filter(|l| *l > 0.0 && *l < 100.0)
        .map(|l| l as u8);

    let potencia_w = if por_energia {
        ritmo.map(|p| p / 1e6)
    } else {
        ritmo.zip(num("VOLTAGE_NOW")).map(|(i, v)| i * v / 1e12)
    };
    let a_wh = |x: f64| {
        if por_energia {
            Some(x / 1e6)
        } else {
            num("VOLTAGE_MIN_DESIGN").map(|v| x * v / 1e12)
        }
    };

    let minutos_restantes = match (&estado, ahora, llena, ritmo) {
        (Estado::Descargando, Some(a), _, Some(r)) => Some(a / r * 60.0),
        (Estado::Cargando, Some(a), Some(l), Some(r)) => {
            let objetivo = l * f64::from(limite.unwrap_or(100)) / 100.0;
            Some((objetivo - a).max(0.0) / r * 60.0)
        }
        _ => None,
    };

    Some(Bateria {
        porcentaje: porcentaje as f32,
        estado,
        conectado: false,
        // El medidor de carga reaprende la capacidad con el uso y puede pasarse
        // de la de diseño: este Galaxy Book da 4 100 000 µAh llena frente a
        // 4 023 000 µAh de diseño (101,9 %). La batería no está «mejor que
        // nueva»; la cifra real se sigue viendo en la capacidad.
        salud: llena
            .zip(diseno)
            .filter(|(_, d)| *d > 0.0)
            .map(|(l, d)| (l / d * 100.0).min(100.0) as f32),
        ciclos: num("CYCLE_COUNT").filter(|n| *n > 0.0).map(|n| n as u32),
        potencia_w: potencia_w.map(|p| p as f32),
        minutos_restantes: minutos_restantes.map(|m| m.round() as u32),
        limite_carga: limite,
        capacidad_wh: llena.and_then(a_wh).map(|w| w as f32),
        capacidad_diseno_wh: diseno.and_then(a_wh).map(|w| w as f32),
        tecnologia: texto("TECHNOLOGY"),
        fabricante: texto("MANUFACTURER"),
        modelo: texto("MODEL_NAME"),
        serie: texto("SERIAL_NUMBER"),
        energia_wh: ahora.and_then(a_wh).map(|w| w as f32),
        tension_v: num("VOLTAGE_NOW").map(|v| (v / 1e6) as f32),
        tension_diseno_v: num("VOLTAGE_MIN_DESIGN").map(|v| (v / 1e6) as f32),
        // Décimas de grado; pocos firmwares lo publican.
        temperatura_c: num("TEMP").map(|t| (t / 10.0) as f32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Copiado de /sys/class/power_supply/BAT1/uevent de un Galaxy Book5 Pro.
    const GALAXY_BOOK: &str = "\
DEVTYPE=power_supply
POWER_SUPPLY_NAME=BAT1
POWER_SUPPLY_TYPE=Battery
POWER_SUPPLY_STATUS=Charging
POWER_SUPPLY_PRESENT=1
POWER_SUPPLY_TECHNOLOGY=Li-ion
POWER_SUPPLY_CYCLE_COUNT=239
POWER_SUPPLY_VOLTAGE_MIN_DESIGN=15440000
POWER_SUPPLY_VOLTAGE_NOW=17234000
POWER_SUPPLY_CURRENT_NOW=2799000
POWER_SUPPLY_CHARGE_FULL_DESIGN=4023000
POWER_SUPPLY_CHARGE_FULL=4100000
POWER_SUPPLY_CHARGE_NOW=3075000
POWER_SUPPLY_CHARGE_CONTROL_END_THRESHOLD=80
POWER_SUPPLY_CAPACITY=75
POWER_SUPPLY_MANUFACTURER=SAMSUNG Electronics
";

    #[test]
    fn bateria_por_carga_calcula_potencia_y_tiempo_hasta_el_limite() {
        let b = desde_uevent(&campos_uevent(GALAXY_BOOK)).expect("hay CAPACITY");
        assert_eq!(b.estado, Estado::Cargando);
        assert_eq!(b.limite_carga, Some(80));
        assert_eq!(b.ciclos, Some(239));
        // 2,799 A · 17,234 V
        assert!((b.potencia_w.expect("hay corriente y tensión") - 48.24).abs() < 0.01);
        // (4,1 Ah · 0,8 − 3,075 Ah) / 2,799 A = 0,0732 h ≈ 4 min
        assert_eq!(b.minutos_restantes, Some(4));
        // Llena 4,1 Ah sobre 4,023 Ah de diseño: 101,9 %, que se queda en 100.
        assert_eq!(b.salud, Some(100.0));
        // 4,1 Ah · 15,44 V
        assert!((b.capacidad_wh.expect("hay tensión nominal") - 63.3).abs() < 0.1);
    }

    #[test]
    fn bateria_por_energia_descargando() {
        let texto = "POWER_SUPPLY_STATUS=Discharging\nPOWER_SUPPLY_CAPACITY=50\n\
                     POWER_SUPPLY_ENERGY_NOW=30000000\nPOWER_SUPPLY_ENERGY_FULL=60000000\n\
                     POWER_SUPPLY_POWER_NOW=10000000\n";
        let b = desde_uevent(&campos_uevent(texto)).expect("hay CAPACITY");
        assert_eq!(b.minutos_restantes, Some(180));
        assert_eq!(b.potencia_w, Some(10.0));
        assert_eq!(b.salud, None);
    }
}
