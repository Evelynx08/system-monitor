//! Hardware concreto de cada sección, sin permisos de administrador.
//!
//! Sale de sysfs, /proc y de la base de datos de udev, que al arrancar copia
//! de la tabla DMI los datos de los módulos de memoria que dmidecode solo deja
//! leer a root. Los nombres comerciales vienen de pci.ids y usb.ids (hwdata).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::gpu::buscar_pci;

#[derive(Serialize)]
pub struct Hardware {
    equipo: Equipo,
    cpu: Cpu,
    gpus: Vec<Gpu>,
    ram: Ram,
    discos: Vec<Disco>,
    red: Vec<Red>,
    otros: Otros,
}

/// Lo que no cae en ninguna de las otras secciones: pantallas, cámaras,
/// tarjetas de sonido y adaptadores Bluetooth.
#[derive(Serialize)]
pub struct Otros {
    pantallas: Vec<Pantalla>,
    camaras: Vec<Dispositivo>,
    sonido: Vec<String>,
    bluetooth: Vec<String>,
}

#[derive(Serialize)]
pub struct Pantalla {
    conector: String,
    conectada: bool,
    resolucion: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Dispositivo {
    nombre: String,
    cuantos: usize,
}

#[derive(Serialize)]
pub struct Equipo {
    fabricante: Option<String>,
    modelo: Option<String>,
    placa: Option<String>,
    bios: Option<String>,
    nucleo: Option<String>,
}

#[derive(Serialize)]
pub struct Cpu {
    fabricante: Option<String>,
    arquitectura: &'static str,
    nucleos: usize,
    hilos: usize,
    nucleos_rendimiento: Option<usize>,
    nucleos_eficiencia: Option<usize>,
    cache: Vec<Cache>,
    frecuencia_min_mhz: Option<u32>,
    frecuencia_max_mhz: Option<u32>,
    controlador: Option<String>,
    gobernador: Option<String>,
    microcodigo: Option<String>,
    /// Familia/modelo/stepping, lo que identifica la revisión exacta del chip.
    identificador: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Cache {
    nivel: u8,
    tipo: String,
    bytes: u64,
}

#[derive(Serialize)]
pub struct Gpu {
    nombre: String,
    fabricante: Option<String>,
    /// Quien montó la tarjeta o el portátil (subsistema PCI).
    ensamblador: Option<String>,
    controlador: Option<String>,
    pci: String,
    enlace: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Ram {
    maxima: Option<u64>,
    modulos: Vec<Modulo>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Modulo {
    ubicacion: Option<String>,
    bytes: u64,
    tipo: Option<String>,
    velocidad_mts: Option<u32>,
    fabricante: Option<String>,
    formato: Option<String>,
    rangos: Option<u8>,
}

#[derive(Serialize)]
pub struct Disco {
    dispositivo: String,
    modelo: Option<String>,
    serie: Option<String>,
    firmware: Option<String>,
    conexion: Option<&'static str>,
    bytes: u64,
    rotacional: bool,
    extraible: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TipoRed {
    Wifi,
    Ethernet,
    Otra,
}

#[derive(Serialize)]
pub struct Red {
    interfaz: String,
    nombre: Option<String>,
    controlador: Option<String>,
    tipo: TipoRed,
    bus: Option<&'static str>,
}

struct Ids {
    pci: String,
    usb: String,
}

pub fn leer() -> Hardware {
    let ids = Ids {
        pci: leer_primero(&["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"]),
        usb: leer_primero(&[
            "/usr/share/hwdata/usb.ids",
            "/usr/share/misc/usb.ids",
            "/var/lib/usbutils/usb.ids",
        ]),
    };
    Hardware {
        equipo: equipo(),
        cpu: cpu(),
        gpus: gpus(&ids),
        ram: parsear_ram(&fs::read_to_string("/run/udev/data/+dmi:id").unwrap_or_default()),
        discos: discos(),
        red: red(&ids),
        otros: Otros {
            pantallas: pantallas(),
            camaras: agrupar_camaras(&nombres_camaras()),
            sonido: sonido(&fs::read_to_string("/proc/asound/cards").unwrap_or_default()),
            bluetooth: bluetooth(&ids.usb),
        },
    }
}

fn pantallas() -> Vec<Pantalla> {
    let mut pantallas: Vec<Pantalla> = fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            // «card1-eDP-1»: el conector va detrás del nombre de la tarjeta.
            // Lo que no lleva guion es la tarjeta misma o el nodo de render.
            let conector = e.file_name().into_string().ok()?.split_once('-')?.1.to_string();
            let estado = texto(e.path().join("status"))?;
            Some(Pantalla {
                conector,
                conectada: estado == "connected",
                resolucion: fs::read_to_string(e.path().join("modes"))
                    .ok()
                    .and_then(|m| util(m.lines().next()?)),
            })
        })
        .collect();
    pantallas.sort_by(|a, b| (!a.conectada, &a.conector).cmp(&(!b.conectada, &b.conector)));
    pantallas
}

fn nombres_camaras() -> Vec<String> {
    fs::read_dir("/sys/class/video4linux")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| texto(e.path().join("name")))
        .collect()
}

/// Una cámara aparece como muchos nodos casi iguales (este equipo publica 32
/// «Intel IPU7 ISYS Capture N»): se agrupan por el nombre sin su número.
fn agrupar_camaras(nombres: &[String]) -> Vec<Dispositivo> {
    let mut cuenta: BTreeMap<String, usize> = BTreeMap::new();
    for nombre in nombres {
        let base = nombre.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '-');
        *cuenta
            .entry(if base.is_empty() { nombre.clone() } else { base.to_string() })
            .or_default() += 1;
    }
    cuenta
        .into_iter()
        .map(|(nombre, cuantos)| Dispositivo { nombre, cuantos })
        .collect()
}

/// /proc/asound/cards: «0 [sofhdadsp ]: sof-hda-dsp - sof-hda-dsp».
fn sonido(cards: &str) -> Vec<String> {
    cards
        .lines()
        .filter_map(|l| util(l.split(" - ").nth(1)?))
        .collect()
}

fn bluetooth(ids_usb: &str) -> Vec<String> {
    let mut adaptadores: Vec<String> = fs::read_dir("/sys/class/bluetooth")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let hci = e.file_name().into_string().ok()?;
            // Casi siempre cuelga de USB, y ahí el nombre está un nivel por
            // encima de la interfaz a la que apunta «device».
            let nombre = fs::canonicalize(e.path().join("device"))
                .ok()
                .and_then(|d| d.parent().map(Path::to_path_buf))
                .and_then(|usb| {
                    hex(usb.join("idVendor"))
                        .zip(hex(usb.join("idProduct")))
                        .and_then(|(v, p)| buscar_pci(ids_usb, v, p))
                        .or_else(|| texto(usb.join("product")))
                });
            Some(match nombre {
                Some(n) => format!("{hci} · {n}"),
                None => hci,
            })
        })
        .collect();
    adaptadores.sort();
    adaptadores
}

fn leer_primero(rutas: &[&str]) -> String {
    rutas
        .iter()
        .find_map(|r| fs::read_to_string(r).ok())
        .unwrap_or_default()
}

/// Los fabricantes rellenan la DMI con textos de plantilla; eso no es un dato.
fn util(valor: &str) -> Option<String> {
    const RELLENO: [&str; 10] = [
        "",
        "To Be Filled By O.E.M.",
        "Default string",
        "System Product Name",
        "System manufacturer",
        "Not Specified",
        "Unknown",
        "None",
        "O.E.M.",
        "Not Applicable",
    ];
    let v = valor.trim();
    (!RELLENO.iter().any(|r| r.eq_ignore_ascii_case(v))).then(|| v.to_string())
}

fn texto(ruta: impl AsRef<Path>) -> Option<String> {
    util(&fs::read_to_string(ruta).ok()?)
}

fn numero<T: std::str::FromStr>(ruta: impl AsRef<Path>) -> Option<T> {
    fs::read_to_string(ruta).ok()?.trim().parse().ok()
}

fn hex(ruta: impl AsRef<Path>) -> Option<u16> {
    let t = fs::read_to_string(ruta).ok()?;
    u16::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()
}

fn controlador(dispositivo: &Path) -> Option<String> {
    let enlace = fs::read_link(dispositivo.join("driver")).ok()?;
    enlace.file_name()?.to_str().map(str::to_string)
}

fn es_numerado(nombre: &str, prefijo: &str) -> bool {
    nombre
        .strip_prefix(prefijo)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn equipo() -> Equipo {
    let dmi = |f: &str| texto(Path::new("/sys/class/dmi/id").join(f));
    let unir = |partes: &[Option<String>]| {
        let v: Vec<&str> = partes.iter().flatten().map(String::as_str).collect();
        (!v.is_empty()).then(|| v.join(" "))
    };
    // «Galaxy Book5 Pro (940XHA)»: la familia es el nombre comercial y el
    // producto el código exacto; en sobremesas suele venir solo uno.
    let modelo = match (dmi("product_family"), dmi("product_name")) {
        (Some(familia), Some(nombre)) if familia != nombre => Some(format!("{familia} ({nombre})")),
        (familia, nombre) => familia.or(nombre),
    };
    Equipo {
        fabricante: dmi("sys_vendor"),
        modelo,
        placa: unir(&[dmi("board_vendor"), dmi("board_name")]),
        bios: unir(&[
            dmi("bios_vendor"),
            dmi("bios_version"),
            dmi("bios_date").map(|d| format!("({d})")),
        ]),
        nucleo: texto("/proc/sys/kernel/osrelease").map(|k| format!("Linux {k}")),
    }
}

fn cpu() -> Cpu {
    let info = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let campo = |clave: &str| {
        info.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == clave).then(|| v.trim().to_string())
        })
    };
    let base = Path::new("/sys/devices/system/cpu");
    let hilos: Vec<PathBuf> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| es_numerado(n, "cpu"))
        })
        .map(|e| e.path())
        .filter(|p| p.join("topology").exists())
        .collect();
    // Un núcleo físico es un grupo de hilos hermanos: contar grupos distintos
    // vale igual en Intel con o sin Hyper-Threading, AMD y ARM.
    let nucleos_de = |cpus: &mut dyn Iterator<Item = PathBuf>| {
        cpus.filter_map(|c| fs::read_to_string(c.join("topology/core_cpus_list")).ok())
            .map(|s| s.trim().to_string())
            .collect::<BTreeSet<_>>()
            .len()
    };
    // Intel híbrido (Alder Lake en adelante) separa los tipos de núcleo en dos
    // PMU; el resto de CPU no tiene estos ficheros.
    let tipo_nucleo = |pmu: &str| {
        let lista = texto(Path::new("/sys/devices").join(pmu).join("cpus"))?;
        Some(nucleos_de(
            &mut ids_cpulist(&lista)
                .into_iter()
                .map(|i| base.join(format!("cpu{i}"))),
        ))
    };
    let frecuencias = |fichero: &str| {
        hilos
            .iter()
            .filter_map(|c| numero::<u32>(c.join("cpufreq").join(fichero)))
            .collect::<Vec<_>>()
    };
    let mut cache: Vec<Cache> = fs::read_dir(base.join("cpu0/cache"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("index"))
        .filter_map(|e| {
            let d = e.path();
            Some(Cache {
                nivel: numero(d.join("level"))?,
                tipo: texto(d.join("type"))?,
                bytes: tamano_cache(&texto(d.join("size"))?)?,
            })
        })
        .collect();
    cache.sort_by(|a, b| (a.nivel, &a.tipo).cmp(&(b.nivel, &b.tipo)));

    Cpu {
        fabricante: campo("vendor_id").map(|v| match v.as_str() {
            "GenuineIntel" => "Intel".to_string(),
            "AuthenticAMD" => "AMD".to_string(),
            _ => v,
        }),
        arquitectura: std::env::consts::ARCH,
        nucleos: nucleos_de(&mut hilos.iter().cloned()),
        hilos: hilos.len(),
        nucleos_rendimiento: tipo_nucleo("cpu_core"),
        nucleos_eficiencia: tipo_nucleo("cpu_atom"),
        cache,
        frecuencia_min_mhz: frecuencias("cpuinfo_min_freq")
            .into_iter()
            .min()
            .map(|k| k / 1000),
        frecuencia_max_mhz: frecuencias("cpuinfo_max_freq")
            .into_iter()
            .max()
            .map(|k| k / 1000),
        controlador: texto(base.join("cpu0/cpufreq/scaling_driver")),
        gobernador: texto(base.join("cpu0/cpufreq/scaling_governor")),
        microcodigo: campo("microcode"),
        identificador: match (campo("cpu family"), campo("model"), campo("stepping")) {
            (Some(f), Some(m), Some(s)) => Some(format!("{f}/{m}/{s}")),
            _ => None,
        },
    }
}

/// «0-3,8,10-11» → [0, 1, 2, 3, 8, 10, 11]
fn ids_cpulist(lista: &str) -> Vec<usize> {
    lista
        .trim()
        .split(',')
        .filter_map(|tramo| match tramo.split_once('-') {
            Some((a, b)) => Some((a.parse().ok()?..=b.parse().ok()?).collect::<Vec<usize>>()),
            None => Some(vec![tramo.parse().ok()?]),
        })
        .flatten()
        .collect()
}

/// sysfs escribe «48K», «12288K» o «1M».
fn tamano_cache(texto: &str) -> Option<u64> {
    let (numero, unidad) = texto.split_at(texto.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = numero.parse().ok()?;
    Some(match unidad {
        "K" => n << 10,
        "M" => n << 20,
        "G" => n << 30,
        _ => return None,
    })
}

fn fabricante_ids(ids: &str, vendor: u16) -> Option<String> {
    let cabecera = format!("{vendor:04x} ");
    ids.lines()
        .find_map(|l| l.strip_prefix(&cabecera))
        .map(|n| n.trim().to_string())
}

/// En pci.ids, bajo cada dispositivo, «\t\t<sub_vendor> <sub_device>  Nombre».
fn subsistema_ids(
    ids: &str,
    vendor: u16,
    device: u16,
    sub_vendor: u16,
    sub_device: u16,
) -> Option<String> {
    let cab_vendor = format!("{vendor:04x} ");
    let cab_device = format!("\t{device:04x} ");
    let cab_sub = format!("\t\t{sub_vendor:04x} {sub_device:04x} ");
    let dentro = |l: &&str| l.starts_with('\t') || l.starts_with('#') || l.is_empty();
    ids.lines()
        .skip_while(|l| !l.starts_with(&cab_vendor))
        .skip(1)
        .take_while(dentro)
        .skip_while(|l| !l.starts_with(&cab_device))
        .skip(1)
        .take_while(|l| l.starts_with("\t\t") || l.starts_with('#') || l.is_empty())
        .find_map(|l| l.strip_prefix(&cab_sub))
        .map(|n| n.trim().to_string())
}

fn gpus(ids: &Ids) -> Vec<Gpu> {
    let mut tarjetas: Vec<PathBuf> = fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| es_numerado(n, "card"))
        })
        .map(|e| e.path().join("device"))
        .collect();
    tarjetas.sort();
    tarjetas
        .iter()
        .filter_map(|dev| {
            let pci = fs::canonicalize(dev)
                .ok()?
                .file_name()?
                .to_str()?
                .to_string();
            let (vendor, device) = (hex(dev.join("vendor"))?, hex(dev.join("device"))?);
            let ensamblador = hex(dev.join("subsystem_vendor"))
                .zip(hex(dev.join("subsystem_device")))
                .and_then(|(sv, sd)| {
                    subsistema_ids(&ids.pci, vendor, device, sv, sd).or_else(|| {
                        (sv != vendor)
                            .then(|| fabricante_ids(&ids.pci, sv))
                            .flatten()
                    })
                });
            let enlace = texto(dev.join("current_link_speed")).and_then(|velocidad| {
                let ancho: u32 = numero(dev.join("current_link_width")).filter(|a| *a > 0)?;
                Some(format!("{velocidad} ×{ancho}"))
            });
            Some(Gpu {
                nombre: buscar_pci(&ids.pci, vendor, device)
                    .unwrap_or_else(|| format!("{vendor:04x}:{device:04x}")),
                fabricante: fabricante_ids(&ids.pci, vendor),
                ensamblador,
                controlador: controlador(dev),
                pci,
                enlace,
            })
        })
        .collect()
}

fn parsear_ram(texto_udev: &str) -> Ram {
    let props: HashMap<&str, &str> = texto_udev
        .lines()
        .filter_map(|l| l.strip_prefix("E:")?.split_once('='))
        .collect();
    let indices: BTreeSet<u32> = props
        .keys()
        .filter_map(|k| {
            k.strip_prefix("MEMORY_DEVICE_")?
                .split_once('_')?
                .0
                .parse()
                .ok()
        })
        .collect();
    let modulos = indices
        .into_iter()
        .filter_map(|i| {
            let p = |campo: &str| {
                props
                    .get(format!("MEMORY_DEVICE_{i}_{campo}").as_str())
                    .and_then(|v| util(v))
            };
            // Las ranuras vacías no traen tamaño.
            let bytes = p("SIZE")?.parse().ok().filter(|b: &u64| *b > 0)?;
            Some(Modulo {
                ubicacion: p("LOCATOR"),
                bytes,
                tipo: p("TYPE"),
                velocidad_mts: p("CONFIGURED_SPEED_MTS")
                    .or_else(|| p("SPEED_MTS"))
                    .and_then(|v| v.parse().ok()),
                fabricante: p("MANUFACTURER"),
                formato: p("FORM_FACTOR"),
                rangos: p("RANK").and_then(|v| v.parse().ok()),
            })
        })
        .collect();
    Ram {
        maxima: props
            .get("MEMORY_ARRAY_MAX_CAPACITY")
            .and_then(|v| v.parse().ok()),
        modulos,
    }
}

/// Por dónde cuelga el disco, deducido de su ruta real en sysfs. Primero USB:
/// un disco USB también está detrás de un controlador PCI.
fn conexion(ruta: &str) -> Option<&'static str> {
    [
        ("/usb", "USB"),
        ("/nvme/", "NVMe"),
        ("/mmc_host/", "MMC"),
        ("/virtio", "VirtIO"),
        ("/ata", "SATA"),
    ]
    .iter()
    .find(|(marca, _)| ruta.contains(marca))
    .map(|(_, nombre)| *nombre)
}

fn discos() -> Vec<Disco> {
    let mut discos: Vec<Disco> = fs::read_dir("/sys/block")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let nombre = e.file_name().into_string().ok()?;
            let virtual_ = ["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd"]
                .iter()
                .any(|p| nombre.starts_with(p));
            if virtual_ {
                return None;
            }
            let bloque = e.path();
            let dev = bloque.join("device");
            let sectores: u64 = numero(bloque.join("size")).filter(|s| *s > 0)?;
            let real = fs::canonicalize(&bloque).ok()?;
            Some(Disco {
                modelo: texto(dev.join("model")),
                serie: texto(dev.join("serial")),
                firmware: texto(dev.join("firmware_rev")).or_else(|| texto(dev.join("rev"))),
                conexion: conexion(&real.to_string_lossy()),
                bytes: sectores * 512,
                rotacional: numero::<u8>(bloque.join("queue/rotational")) == Some(1),
                extraible: numero::<u8>(bloque.join("removable")) == Some(1),
                dispositivo: nombre,
            })
        })
        .collect();
    discos.sort_by(|a, b| a.dispositivo.cmp(&b.dispositivo));
    discos
}

fn red(ids: &Ids) -> Vec<Red> {
    let mut red: Vec<Red> = fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let base = e.path();
            let dev = base.join("device");
            // Sin enlace «device» la interfaz es virtual (lo, VPN, puentes).
            let real = fs::canonicalize(&dev).ok()?;
            let tipo = if base.join("wireless").exists() || base.join("phy80211").exists() {
                TipoRed::Wifi
            } else if numero::<u32>(base.join("type")) == Some(1) {
                TipoRed::Ethernet
            } else {
                TipoRed::Otra
            };
            let (nombre, bus) = match (hex(dev.join("vendor")), hex(dev.join("device"))) {
                (Some(v), Some(d)) => (buscar_pci(&ids.pci, v, d), Some("PCIe")),
                // USB: los ids están en el dispositivo, un nivel por encima de
                // la interfaz a la que apunta «device».
                _ => {
                    let usb = real.parent();
                    let nombre = usb.and_then(|u| {
                        hex(u.join("idVendor"))
                            .zip(hex(u.join("idProduct")))
                            .and_then(|(v, d)| buscar_pci(&ids.usb, v, d))
                            .or_else(|| texto(u.join("product")))
                    });
                    (
                        nombre,
                        real.to_string_lossy().contains("/usb").then_some("USB"),
                    )
                }
            };
            Some(Red {
                interfaz: e.file_name().into_string().ok()?,
                nombre,
                controlador: controlador(&dev),
                tipo,
                bus,
            })
        })
        .collect();
    red.sort_by(|a, b| a.interfaz.cmp(&b.interfaz));
    red
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modulos_de_ram_desde_udev() {
        // /run/udev/data/+dmi:id de un Galaxy Book5 Pro, recortado.
        let texto = "E:MEMORY_ARRAY_MAX_CAPACITY=34359738368\n\
E:MEMORY_DEVICE_0_SIZE=4294967296\nE:MEMORY_DEVICE_0_FORM_FACTOR=Row Of Chips\n\
E:MEMORY_DEVICE_0_LOCATOR=Controller0-ChannelA\nE:MEMORY_DEVICE_0_TYPE=LPDDR5\n\
E:MEMORY_DEVICE_0_SPEED_MTS=8533\nE:MEMORY_DEVICE_0_MANUFACTURER=Micron Technology\n\
E:MEMORY_DEVICE_0_RANK=2\nE:MEMORY_DEVICE_0_CONFIGURED_SPEED_MTS=8533\n\
E:MEMORY_DEVICE_1_PRESENT=0\nE:MEMORY_DEVICE_1_LOCATOR=DIMM 1\nE:MEMORY_DEVICE_1_MANUFACTURER=Unknown\n";
        let ram = parsear_ram(texto);
        assert_eq!(ram.maxima, Some(34359738368));
        assert_eq!(
            ram.modulos,
            vec![Modulo {
                ubicacion: Some("Controller0-ChannelA".into()),
                bytes: 4294967296,
                tipo: Some("LPDDR5".into()),
                velocidad_mts: Some(8533),
                fabricante: Some("Micron Technology".into()),
                formato: Some("Row Of Chips".into()),
                rangos: Some(2),
            }]
        );
    }

    #[test]
    fn textos_de_plantilla_no_son_datos() {
        assert_eq!(util("  To Be Filled By O.E.M. \n"), None);
        assert_eq!(util("default string"), None);
        assert_eq!(
            util("Galaxy Book5 Pro\n").as_deref(),
            Some("Galaxy Book5 Pro")
        );
    }

    #[test]
    fn listas_de_cpu_y_cache() {
        assert_eq!(ids_cpulist("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(tamano_cache("12288K"), Some(12288 << 10));
        assert_eq!(tamano_cache("1M"), Some(1 << 20));
        assert_eq!(tamano_cache("48"), None);
    }

    #[test]
    fn conexion_del_disco_por_su_ruta() {
        assert_eq!(
            conexion("/sys/devices/pci0000:00/0000:00:1c.0/0000:55:00.0/nvme/nvme0/nvme0n1"),
            Some("NVMe")
        );
        assert_eq!(
            conexion("/sys/devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host0/target0:0:0/0:0:0:0/block/sda"),
            Some("USB")
        );
        assert_eq!(
            conexion(
                "/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda"
            ),
            Some("SATA")
        );
    }

    #[test]
    fn ids_de_fabricante_y_subsistema() {
        let ids = "144d  Samsung Electronics Co Ltd\n8086  Intel Corporation\n\
\t64a0  Core Ultra 200V Series Processors Arc Graphics 130V/140V GPU\n\
\t\t144d ca08  Galaxy Book5 Pro\n\t64b0  Lunar Lake [Intel Graphics]\n\t\t144d ca09  Otro\n8087  Intel Corp.\n";
        assert_eq!(
            fabricante_ids(ids, 0x144d).as_deref(),
            Some("Samsung Electronics Co Ltd")
        );
        assert_eq!(
            subsistema_ids(ids, 0x8086, 0x64a0, 0x144d, 0xca08).as_deref(),
            Some("Galaxy Book5 Pro")
        );
        // El subsistema de otro dispositivo no vale.
        assert_eq!(subsistema_ids(ids, 0x8086, 0x64a0, 0x144d, 0xca09), None);
    }
}

#[cfg(test)]
mod equipo_real {
    /// `cargo test lee_este_equipo -- --ignored --nocapture` enseña lo que se
    /// lee en la máquina donde se ejecuta: sirve para revisar otro equipo.
    #[test]
    #[ignore]
    fn lee_este_equipo() {
        let hardware =
            serde_json::to_string_pretty(&super::leer()).expect("el hardware se serializa");
        println!("{hardware}");
    }
}
