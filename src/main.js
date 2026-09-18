'use strict';

const { invoke, convertFileSrc } = window.__TAURI__.core;
const ventanaTauri = window.__TAURI__.window.getCurrentWindow();
const i18n = window.BookosI18n;
const t = i18n.t;
const $ = (sel) => document.querySelector(sel);

// ── Zoom desactivado ──────────────────────────────────────────────────────
// El pellizco del touchpad en WebKitGTK no llega aquí (lo apaga main.rs);
// esto cubre Ctrl+rueda, los atajos y el multitáctil de pantallas táctiles.
window.addEventListener('wheel', (e) => {
  if (e.ctrlKey || e.metaKey) e.preventDefault();
}, { passive: false });
window.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && ['+', '-', '=', '0'].includes(e.key)) e.preventDefault();
});
window.addEventListener('touchstart', (e) => {
  if (e.touches.length > 1) e.preventDefault();
}, { passive: false });
for (const tipo of ['gesturestart', 'gesturechange', 'gestureend']) {
  document.addEventListener(tipo, (e) => e.preventDefault());
}

// ── Estado ────────────────────────────────────────────────────────────────
const MUESTRAS = 60;
const ORDEN_SECCIONES = ['cpu', 'gpu', 'ram', 'disco', 'red', 'bateria', 'otros'];
const FILTROS = ['aplicacion', 'sesion', 'segundo_plano', 'sistema', 'todo'];

const estado = {
  seccion: ORDEN_SECCIONES.includes(localStorage.getItem('monitor.seccion'))
    ? localStorage.getItem('monitor.seccion') : 'cpu',
  intervalo: Number(localStorage.getItem('monitor.intervalo')) || 2000,
  filtro: 'aplicacion',
  busqueda: '',
  // Duran lo que la sesión: guardarlos haría que al abrir la app faltaran
  // procesos sin que se viera por qué.
  filtroGrupos: new Set(),
  soloActivos: false,
  soloMios: false,
  orden: null,
  resumen: null,
  grupos: [],
  expandidos: new Set(),
  hilos: new Map(),
  seleccion: null,
  historia: nuevaHistoria(),
  hardware: null,
  // undefined: sin pedir todavía; null: el sistema no guarda historial.
  historial: undefined,
  rangoHistorial: [6, 24, 168].includes(Number(localStorage.getItem('monitor.historial')))
    ? Number(localStorage.getItem('monitor.historial')) : 24,
  historialPedido: 0,
};

function nuevaHistoria() {
  return { cpu: [[], []], gpu: [[]], ram: [[]], disco: [[], []], red: [[], []], bateria: [[]], otros: [[]] };
}

// ── Formatos ──────────────────────────────────────────────────────────────
let f0, f1, f2, espacioPct;
function prepararFormatos() {
  const loc = i18n.getLang();
  f0 = new Intl.NumberFormat(loc, { maximumFractionDigits: 0 });
  f1 = new Intl.NumberFormat(loc, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  f2 = new Intl.NumberFormat(loc, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  espacioPct = loc === 'es' ? ' ' : '';
}
const pct = (v, fmt = f1) => (v == null ? '—' : `${fmt.format(v)}${espacioPct}%`);
// Memoria en base 1024, como la cuentan el kernel y cualquier monitor; tamaños
// de disco y velocidades en base 1000, como los etiquetan fabricantes y redes.
function bytes(n, base = 1024) {
  if (n == null) return '—';
  const unidades = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= base && i < unidades.length - 1) { n /= base; i++; }
  return `${(i === 0 || n >= 100 ? f0 : f1).format(n)} ${unidades[i]}`;
}
const ritmo = (n) => `${bytes(n, 1000)}/s`;
function duracion(seg) {
  const d = Math.floor(seg / 86400);
  const h = Math.floor((seg % 86400) / 3600);
  const m = Math.floor((seg % 3600) / 60);
  if (d) return t('unit.days', { d, h });
  if (h) return t('unit.hmin', { h, m });
  return t('unit.min', { n: m });
}
function poner(el, valor) {
  if (el.textContent !== valor) el.textContent = valor;
}

// ── Iconos ────────────────────────────────────────────────────────────────
// Sistema A de la guía: círculo con el color de la categoría y glifo blanco.
// Lucide no tiene variante rellena, así que el glifo va en trazo, algo más
// grueso que en la UI para que se lea a 30 px. Son los únicos hex fuera de la
// hoja de estilos.
const SECCIONES = {
  cpu: {
    color: '#5856D6',
    glifo: '<path d="M12 20v2"/><path d="M12 2v2"/><path d="M17 20v2"/><path d="M17 2v2"/><path d="M2 12h2"/><path d="M2 17h2"/><path d="M2 7h2"/><path d="M20 12h2"/><path d="M20 17h2"/><path d="M20 7h2"/><path d="M7 20v2"/><path d="M7 2v2"/><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="8" y="8" width="8" height="8" rx="1"/>',
    series: () => [t('cpu.user'), t('cpu.system')],
    apilada: true,
    maximo: 100,
    eje: (v) => pct(v, f0),
    columnas: ['cpu', 'hilos', 'pid'],
  },
  gpu: {
    color: '#AF52DE',
    glifo: '<path d="M2 17h18a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2H2"/><path d="M2 21V3"/><path d="M7 17v3a1 1 0 0 0 1 1h5a1 1 0 0 0 1-1v-3"/><circle cx="16" cy="11" r="2"/><circle cx="8" cy="11" r="2"/>',
    series: () => [t('gpu.usage')],
    maximo: 100,
    eje: (v) => pct(v, f0),
    columnas: ['gpu', 'memoria_gpu', 'pid'],
  },
  ram: {
    color: '#FF9500',
    glifo: '<path d="M12 12v-2"/><path d="M12 18v-2"/><path d="M16 12v-2"/><path d="M16 18v-2"/><path d="M2 11h1.5"/><path d="M20 18v-2"/><path d="M20.5 11H22"/><path d="M4 18v-2"/><path d="M8 12v-2"/><path d="M8 18v-2"/><rect x="2" y="6" width="20" height="10" rx="2"/>',
    series: () => [t('ram.used')],
    maximo: 100,
    eje: (v) => pct(v, f0),
    columnas: ['memoria', 'cpu', 'pid'],
  },
  disco: {
    color: '#30B0C7',
    glifo: '<path d="M10 16h.01"/><path d="M2.212 11.577a2 2 0 0 0-.212.896V18a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-5.527a2 2 0 0 0-.212-.896L18.55 5.11A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/><path d="M21.946 12.013H2.054"/><path d="M6 16h.01"/>',
    series: () => [t('disk.read'), t('disk.write')],
    eje: ritmo,
    columnas: ['lectura', 'escritura', 'pid'],
  },
  red: {
    color: '#007AFF',
    glifo: '<rect x="16" y="16" width="6" height="6" rx="1"/><rect x="2" y="16" width="6" height="6" rx="1"/><rect x="9" y="2" width="6" height="6" rx="1"/><path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3"/><path d="M12 12V8"/>',
    series: () => [t('net.down'), t('net.up')],
    eje: ritmo,
    columnas: null,
  },
  bateria: {
    color: '#34C759',
    glifo: '<path d="M10 14v-4"/><path d="M22 14v-4"/><path d="M6 14v-4"/><rect x="2" y="6" width="16" height="12" rx="2"/>',
    series: () => [t('bat.level')],
    maximo: 100,
    eje: (v) => pct(v, f0),
    columnas: ['impacto', 'memoria', 'pid'],
  },
  otros: {
    color: '#FF375F',
    glifo: '<path d="M14 4v10.54a4 4 0 1 1-4 0V4a2 2 0 0 1 4 0Z"/>',
    series: () => [t('sen.hottest')],
    // Eje fijo a 100 °C: casi todos los chips avisan de crítico ahí y una
    // escala que se mueve haría parecer picos lo que son cambios de dos grados.
    maximo: 100,
    eje: (v) => `${f0.format(v)}\u00a0°C`,
    columnas: null,
  },
};

// Chips de hwmon con nombre entendible; lo que no esté, con el del kernel.
const CHIPS = [
  [/^(coretemp|k10temp|zenpower|cpu_thermal|soc_thermal)/, 'chip.cpu'],
  [/^nvme/, 'chip.nvme'],
  [/^(iwlwifi|mt79|ath1|rtw)/, 'chip.wifi'],
  [/^(amdgpu|xe|i915|nouveau|radeon)/, 'chip.gpu'],
  [/^acpitz/, 'chip.board'],
  [/^(BAT|bat)/, 'sec.bateria'],
  [/^(ADP|AC)/, 'chip.adapter'],
  [/^ucsi/, 'chip.usbc'],
  [/^(acpi_fan|thinkpad|dell_smm|applesmc)/, 'chip.fan'],
  [/^thermal$/, 'chip.zones'],
  [/^(nct|it87|w836)/, 'chip.super'],
];
const nombreChip = (n) => {
  const encontrado = CHIPS.find(([patron]) => patron.test(n));
  return encontrado ? `${t(encontrado[1])} (${n})` : n;
};

// Un sensor está «alto» cerca de su crítico y «crítico» al llegar. Sin umbral
// publicado no se inventa ninguno: se enseña el valor y ya.
function estadoSensor(l) {
  const limite = l.critico ?? l.maximo;
  if (!limite) return { clase: '', fraccion: null };
  const fraccion = l.valor / limite;
  return { clase: fraccion >= 1 ? 'critico' : fraccion >= 0.9 ? 'alto' : '', fraccion };
}

// Sin rpm queda el nivel de refrigeración, que casi siempre es encendido o
// apagado; con más niveles se dice cuál de cuántos.
function estadoVentilador(v) {
  if (v.rpm != null) return `${f0.format(v.rpm)} rpm`;
  if (v.nivel == null) return '—';
  if ((v.nivel_maximo ?? 1) <= 1) return t(v.nivel > 0 ? 'sen.fan_on' : 'sen.fan_off');
  return t('sen.fan_level', { n: v.nivel, max: v.nivel_maximo });
}

// Los modos del firmware se llaman igual que los de power-profiles-daemon,
// salvo «low-power» y «quiet», que ese demonio no expone.
const MODOS = { 'low-power': 'profile.saver', 'power-saver': 'profile.saver', quiet: 'profile.quiet', balanced: 'profile.balanced', performance: 'profile.performance' };
const nombreModo = (m) => (MODOS[m] ? t(MODOS[m]) : m);

// «0»/«1» de un interruptor del firmware se leen mejor como sí o no.
function valorAjuste(a) {
  const binario = a.opciones.length === 2 && a.opciones.includes('0') && a.opciones.includes('1');
  return binario ? t(a.valor === '1' ? 'otros.on' : 'otros.off') : a.valor;
}

function temperaturas(sensores) {
  return sensores.chips.flatMap((c) => c.temperaturas.map((l) => ({ ...l, chip: c.nombre })));
}

const masCaliente = (sensores) => temperaturas(sensores)
  .reduce((mayor, l) => (mayor == null || l.valor > mayor.valor ? l : mayor), null);

const COLUMNAS = {
  cpu: { titulo: 'col.cpu', valor: (p) => p.cpu, texto: (v) => f1.format(v) },
  impacto: { titulo: 'col.impacto', valor: (p) => p.cpu, texto: (v) => f1.format(v) },
  hilos: { titulo: 'col.hilos', valor: (p) => p.hilos, texto: (v) => f0.format(v) },
  pid: { titulo: 'col.pid', valor: (p) => p.pid, texto: (v) => String(v), sinSuma: true },
  memoria: { titulo: 'col.memoria', valor: (p) => p.memoria, texto: (v) => bytes(v) },
  lectura: { titulo: 'col.lectura', valor: (p) => p.lectura, texto: ritmo },
  escritura: { titulo: 'col.escritura', valor: (p) => p.escritura, texto: ritmo },
  gpu: { titulo: 'col.gpu', valor: (p) => p.gpu, texto: (v) => f1.format(v) },
  memoria_gpu: { titulo: 'col.memoria_gpu', valor: (p) => p.memoria_gpu, texto: (v) => bytes(v) },
};

const SVG_TRAZO = 'viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"';
const ICONO_APP = `<svg ${SVG_TRAZO} width="18" height="18"><rect x="2" y="4" width="20" height="16" rx="2"/><path d="M10 4v4"/><path d="M2 8h20"/><path d="M6 4v4"/></svg>`;
const ICONO_SERVICIO = `<svg ${SVG_TRAZO} width="18" height="18"><path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"/><circle cx="12" cy="12" r="3"/></svg>`;
const CHEVRON = `<svg ${SVG_TRAZO} width="14" height="14" stroke-width="2.5"><path d="m9 18 6-6-6-6"/></svg>`;
const FLECHA_ORDEN = `<svg ${SVG_TRAZO} width="12" height="12" stroke-width="2.5"><path d="m6 9 6 6 6-6"/></svg>`;
const ICONO_QUITAR = `<svg ${SVG_TRAZO} width="12" height="12" stroke-width="2.5"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>`;

function iconoSeccion(id) {
  const s = SECCIONES[id];
  return `<svg class="item-icono" viewBox="0 0 30 30" aria-hidden="true"><circle cx="15" cy="15" r="15" fill="${s.color}"/><g transform="translate(6 6) scale(.75)" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">${s.glifo}</g></svg>`;
}

// ── Barra lateral ─────────────────────────────────────────────────────────
const nav = $('#nav');
function crearNav() {
  for (const id of ORDEN_SECCIONES) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'item';
    b.dataset.seccion = id;
    b.innerHTML = `${iconoSeccion(id)}<span class="item-texto"><span class="item-titulo"></span><span class="item-sub"></span></span><span class="item-valor"></span>`;
    b.addEventListener('click', () => cambiarSeccion(id));
    nav.append(b);
  }
}

function pintarNav() {
  const r = estado.resumen;
  for (const b of nav.children) {
    const id = b.dataset.seccion;
    let sub = '';
    let valor = '—';
    if (r) {
      switch (id) {
        case 'cpu':
          sub = r.cpu.modelo;
          valor = pct(r.cpu.uso);
          break;
        case 'gpu': {
          const g = r.gpus[0];
          sub = g ? g.nombre : t('gpu.none');
          valor = g && g.uso != null ? pct(g.uso) : '—';
          break;
        }
        case 'ram':
          sub = `${bytes(r.ram.usada)} ${t('ram.of', { total: bytes(r.ram.total) })}`;
          valor = pct((r.ram.usada / r.ram.total) * 100);
          break;
        case 'disco': {
          const raiz = r.disco.volumenes.find((v) => v.punto_montaje === '/') || r.disco.volumenes[0];
          sub = raiz ? t('disk.free', { free: bytes(raiz.disponible, 1000), total: bytes(raiz.total, 1000) }) : '';
          valor = ritmo(r.disco.lectura + r.disco.escritura);
          break;
        }
        case 'red': {
          const principal = r.red.interfaces.find((i) => !i.es_virtual && i.ip);
          sub = principal ? `${principal.nombre} · ${principal.ip}` : t('net.no_ip');
          valor = `↓ ${ritmo(r.red.bajada)}`;
          break;
        }
        case 'bateria':
          if (r.bateria) {
            const b2 = r.bateria;
            sub = t(`st.${b2.estado}`);
            if (b2.minutos_restantes != null) sub += ` · ${duracion(b2.minutos_restantes * 60)}`;
            valor = pct(b2.porcentaje, f0);
          } else {
            sub = t('bat.none');
          }
          break;
        case 'otros': {
          const caliente = masCaliente(r.sensores);
          const cuantos = r.sensores.chips.reduce((n, c) => n + c.temperaturas.length
            + c.ventiladores.length + c.tensiones.length + c.corrientes.length + c.potencias.length, 0);
          sub = t('sen.count', { n: cuantos });
          if (caliente) valor = `${f0.format(caliente.valor)}\u00a0°C`;
          break;
        }
      }
    }
    poner(b.querySelector('.item-titulo'), t(`sec.${id}`));
    poner(b.querySelector('.item-sub'), sub);
    poner(b.querySelector('.item-valor'), valor);
    if (id === estado.seccion) b.setAttribute('aria-current', 'page');
    else b.removeAttribute('aria-current');
  }
}

// ── Gráfica ───────────────────────────────────────────────────────────────
const lienzo = $('#lienzo');
const canvas = $('#grafica');
const ctx = canvas.getContext('2d');
const cruz = $('#cruz');
const tooltip = $('#tooltip');
let colores = null;
let geometria = null;
let punteroX = null;

// Los colores resueltos solo cambian con la paleta o el tema; leerlos en cada
// fotograma obligaría a recalcular estilos cada vez.
function leerColores() {
  const cs = getComputedStyle(document.documentElement);
  const v = (nombre) => cs.getPropertyValue(nombre).trim();
  colores = { series: [v('--serie-1'), v('--serie-2')], rejilla: v('--div'), texto: v('--tx2'), superficie: v('--card'), banda: v('--sbg') };
}

function escalaRedonda(v) {
  if (v <= 0) return 1000;
  const e = 10 ** Math.floor(Math.log10(v));
  return [1, 2, 5, 10].map((m) => m * e).find((m) => m >= v);
}

function valoresDibujo(seccion) {
  const datos = estado.historia[seccion];
  if (!SECCIONES[seccion].apilada) return datos;
  // Apilada: cada serie se dibuja sobre la suma de las que tiene debajo.
  return datos.map((_, i) => datos[0].map((__, k) =>
    datos.slice(i).reduce((suma, serie) => suma + (serie[k] ?? 0), 0)));
}

const FUENTE_EJE = '11px -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Roboto, sans-serif';

// Línea de 2 px con un velo del 10 % debajo. Los null cortan la línea en vez
// de unir con una recta los extremos de un hueco sin datos.
function trazar(c, valores, xDe, yDe, yBase, color, conPunto) {
  const tramos = [];
  let tramo = [];
  valores.forEach((v, k) => {
    if (v == null) {
      if (tramo.length) tramos.push(tramo);
      tramo = [];
    } else {
      tramo.push([xDe(k), yDe(v)]);
    }
  });
  if (tramo.length) tramos.push(tramo);
  const camino = (puntos) => {
    c.beginPath();
    puntos.forEach(([x, y], j) => (j ? c.lineTo(x, y) : c.moveTo(x, y)));
  };
  c.lineJoin = 'round';
  c.lineCap = 'round';
  for (const puntos of tramos) {
    camino(puntos);
    c.lineTo(puntos[puntos.length - 1][0], yBase);
    c.lineTo(puntos[0][0], yBase);
    c.closePath();
    c.globalAlpha = 0.1;
    c.fillStyle = color;
    c.fill();
    c.globalAlpha = 1;
    camino(puntos);
    c.lineWidth = 2;
    c.strokeStyle = color;
    c.stroke();
  }
  const ultimo = valores[valores.length - 1];
  if (conPunto && ultimo != null) {
    c.beginPath();
    c.arc(xDe(valores.length - 1), yDe(ultimo), 4, 0, Math.PI * 2);
    c.fillStyle = color;
    c.fill();
    c.lineWidth = 2;
    c.strokeStyle = colores.superficie;
    c.stroke();
  }
}

function dibujar() {
  if (!colores) return;
  const s = SECCIONES[estado.seccion];
  const series = valoresDibujo(estado.seccion);
  const dpr = window.devicePixelRatio || 1;
  const ancho = lienzo.clientWidth;
  const alto = lienzo.clientHeight;
  if (canvas.width !== Math.round(ancho * dpr) || canvas.height !== Math.round(alto * dpr)) {
    canvas.width = Math.round(ancho * dpr);
    canvas.height = Math.round(alto * dpr);
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, ancho, alto);

  const x0 = 0;
  const x1 = ancho - 64;
  const y0 = 8;
  const y1 = alto - 8;
  const pico = Math.max(0, ...series.flat().filter((v) => v != null));
  const maximo = s.maximo ?? escalaRedonda(pico);
  const yDe = (v) => y1 - (Math.min(v, maximo) / maximo) * (y1 - y0);
  const paso = (x1 - x0) / (MUESTRAS - 1);
  const xDe = (k, n) => x1 - (n - 1 - k) * paso;
  geometria = { x0, x1, paso, yDe, xDe };

  ctx.lineWidth = 1;
  ctx.strokeStyle = colores.rejilla;
  ctx.fillStyle = colores.texto;
  ctx.font = FUENTE_EJE;
  ctx.textBaseline = 'middle';
  for (const v of [0, maximo / 2, maximo]) {
    const y = Math.round(yDe(v)) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x0, y);
    ctx.lineTo(x1, y);
    ctx.stroke();
    ctx.fillText(s.eje(v), x1 + 8, y);
  }

  series.forEach((valores, i) => {
    trazar(ctx, valores, (k) => xDe(k, valores.length), yDe, y1, colores.series[i], true);
  });
  canvas.setAttribute('aria-label', t('chart.label', { s: Math.round((MUESTRAS * estado.intervalo) / 1000) }));
  pintarTooltip();
}

new ResizeObserver(dibujar).observe(lienzo);
lienzo.addEventListener('pointermove', (e) => {
  punteroX = e.clientX - lienzo.getBoundingClientRect().left;
  pintarTooltip();
});
lienzo.addEventListener('pointerleave', () => {
  punteroX = null;
  cruz.hidden = true;
  tooltip.hidden = true;
});

function pintarTooltip() {
  const datos = estado.historia[estado.seccion];
  const n = datos[0].length;
  if (punteroX == null || !geometria || !n) return;
  const { x1, paso, xDe } = geometria;
  const k = Math.min(n - 1, Math.max(0, n - 1 - Math.round((x1 - punteroX) / paso)));
  const x = xDe(k, n);
  cruz.style.transform = `translateX(${Math.round(x)}px)`;
  cruz.hidden = false;

  const s = SECCIONES[estado.seccion];
  const nombres = s.series();
  const hace = Math.round(((n - 1 - k) * estado.intervalo) / 1000);
  const cuando = document.createElement('div');
  cuando.className = 'tooltip-cuando';
  cuando.textContent = hace ? t('chart.ago', { s: hace }) : t('chart.now');
  const filas = datos.map((serie, i) => {
    const fila = document.createElement('div');
    fila.className = `tooltip-fila clave serie-${i + 1}`;
    const valor = document.createElement('strong');
    if (serie[k] == null) valor.textContent = '—';
    else valor.textContent = s.maximo === 100 ? pct(serie[k]) : s.eje(serie[k]);
    const nombre = document.createElement('span');
    nombre.textContent = nombres[i];
    fila.append(valor, nombre);
    return fila;
  });
  tooltip.replaceChildren(cuando, ...filas);
  tooltip.hidden = false;
  const anchoTip = tooltip.offsetWidth;
  const izquierda = x + 12 + anchoTip > lienzo.clientWidth ? x - 12 - anchoTip : x + 12;
  tooltip.style.transform = `translateX(${Math.round(izquierda)}px)`;
}

function registrar(r) {
  const h = estado.historia;
  const meter = (serie, v) => {
    serie.push(v);
    if (serie.length > MUESTRAS) serie.shift();
  };
  meter(h.cpu[0], r.cpu.usuario);
  meter(h.cpu[1], r.cpu.sistema);
  meter(h.gpu[0], r.gpus[0]?.uso ?? null);
  meter(h.ram[0], (r.ram.usada / r.ram.total) * 100);
  meter(h.disco[0], r.disco.lectura);
  meter(h.disco[1], r.disco.escritura);
  meter(h.red[0], r.red.bajada);
  meter(h.red[1], r.red.subida);
  meter(h.bateria[0], r.bateria?.porcentaje ?? null);
  meter(h.otros[0], masCaliente(r.sensores)?.valor ?? null);
}

// ── Página de detalle ─────────────────────────────────────────────────────
const hero = $('#hero');
const cifras = $('#cifras');
const extra = $('#extra');

function el(etiqueta, clase, texto) {
  const e = document.createElement(etiqueta);
  if (clase) e.className = clase;
  if (texto != null) e.textContent = texto;
  return e;
}

function pintarCifras(principal, lista) {
  const nodos = [];
  const heroNodo = el('div', 'cifra cifra-hero');
  const dd = el('dd', null, principal.valor);
  if (principal.sub) dd.append(' ', el('span', 'cifra-sub', principal.sub));
  heroNodo.append(el('dt', null, principal.etiqueta), dd);
  nodos.push(heroNodo);
  for (const [etiqueta, valor] of lista) {
    const c = el('div', 'cifra');
    c.append(el('dt', null, etiqueta), el('dd', null, valor));
    nodos.push(c);
  }
  cifras.replaceChildren(...nodos);
}

function medidor(fraccion, clase = 'serie-1') {
  const m = el('div', `medidor ${clase}`);
  const relleno = el('div', 'medidor-relleno');
  relleno.style.transform = `scaleX(${Math.min(1, Math.max(0, fraccion))})`;
  m.append(relleno);
  return m;
}

function pintarLeyenda(sec) {
  const nombres = SECCIONES[sec].series();
  // Una sola serie no lleva leyenda: el título ya dice qué se dibuja.
  $('#leyenda').replaceChildren(...(nombres.length > 1
    ? nombres.map((n, i) => el('span', `clave serie-${i + 1}`, n))
    : []));
}

function sinDatos(mensaje) {
  hero.classList.toggle('sin-datos', mensaje != null);
  const vacio = $('#vacio');
  vacio.hidden = mensaje == null;
  if (mensaje != null) poner(vacio, mensaje);
}

function pintarPagina() {
  const r = estado.resumen;
  const sec = estado.seccion;
  poner($('#titulo'), t(`sec.${sec}`));
  pintarLeyenda(sec);
  if (!r) return;
  const bloques = [];
  let subtitulo = '';
  sinDatos(null);

  switch (sec) {
    case 'cpu': {
      const c = r.cpu;
      subtitulo = `${c.modelo} · ${c.nucleos.length} ${t('cpu.cores').toLowerCase()}`;
      pintarCifras({ etiqueta: t('sec.cpu'), valor: pct(c.uso) }, [
        [t('cpu.user'), pct(c.usuario)],
        [t('cpu.system'), pct(c.sistema)],
        [t('cpu.idle'), pct(Math.max(0, 100 - c.uso))],
        // Con dos decimales no cabe en la media columna (se veía «1,68 · 1,52 · 1…»).
        [t('cpu.load'), c.carga.map((v) => f1.format(v)).join(' · ')],
        [t('cpu.freq'), c.frecuencia_mhz != null ? `${f2.format(c.frecuencia_mhz / 1000)} GHz` : '—'],
        [t('cpu.temp'), c.temperatura != null ? `${f0.format(c.temperatura)} °C` : '—'],
        [t('cpu.procs'), f0.format(c.procesos)],
        [t('cpu.threads'), f0.format(c.hilos)],
        [t('cpu.uptime'), duracion(c.activo_seg)],
      ]);
      const tarjeta = el('section', 'card bloque');
      const rejilla = el('div', 'nucleos');
      c.nucleos.forEach((uso, i) => {
        const n = el('div', 'nucleo');
        const linea = el('div', 'nucleo-linea');
        linea.append(el('span', null, t('cpu.core', { n: i + 1 })), el('span', null, pct(uso, f0)));
        n.append(linea, medidor(uso / 100));
        rejilla.append(n);
      });
      tarjeta.append(rejilla);
      bloques.push(tarjeta);
      break;
    }
    case 'gpu': {
      const g = r.gpus[0];
      if (!g) {
        sinDatos(t('gpu.none'));
        break;
      }
      subtitulo = r.gpus.map((x) => x.nombre).join(' · ');
      const lista = [
        [t('gpu.driver'), g.controlador],
        [t('gpu.freq'), g.frecuencia_mhz != null
          ? `${f0.format(g.frecuencia_mhz)}${g.frecuencia_max_mhz ? ` / ${f0.format(g.frecuencia_max_mhz)}` : ''} MHz`
          : '—'],
      ];
      if (g.vram_total) lista.push([t('gpu.vram'), `${bytes(g.vram_usada)} ${t('ram.of', { total: bytes(g.vram_total) })}`]);
      if (g.temperatura != null) lista.push([t('gpu.temp'), `${f0.format(g.temperatura)} °C`]);
      pintarCifras(g.uso != null
        ? { etiqueta: t('gpu.usage'), valor: pct(g.uso) }
        : { etiqueta: t('gpu.usage'), valor: t('gpu.no_usage'), sub: t('gpu.no_usage_sub') }, lista);
      break;
    }
    case 'ram': {
      const m = r.ram;
      const lista = [
        [t('ram.used'), bytes(m.usada)],
        [t('ram.cache'), bytes(m.cache)],
        [t('ram.available'), bytes(m.disponible)],
      ];
      if (m.comprimida != null) lista.push([t('ram.compressed'), bytes(m.comprimida)]);
      lista.push([t('ram.swap'), `${bytes(m.swap_usada)} ${t('ram.of', { total: bytes(m.swap_total) })}`]);
      if (m.presion != null) lista.push([t('ram.pressure'), pct(m.presion)]);
      pintarCifras({ etiqueta: t('ram.used'), valor: bytes(m.usada), sub: t('ram.of', { total: bytes(m.total) }) }, lista);

      // Composición: usada, caché y libre. Cada tramo lleva su valor en la
      // leyenda de abajo; el color nunca va solo.
      const tarjeta = el('section', 'card bloque');
      const barra = el('div', 'barra-ram');
      const libre = Math.max(0, m.total - m.usada - m.cache);
      [[m.usada, 'serie-1'], [m.cache, 'serie-2'], [libre, 'libre']].forEach(([v, clase]) => {
        const tramo = el('span', clase);
        tramo.style.flexGrow = String(v);
        barra.append(tramo);
      });
      const leyenda = el('div', 'leyenda');
      leyenda.append(
        el('span', 'clave serie-1', `${t('ram.used')} ${bytes(m.usada)}`),
        el('span', 'clave serie-2', `${t('ram.cache')} ${bytes(m.cache)}`),
        el('span', null, `${t('ram.available')} ${bytes(m.disponible)}`),
      );
      tarjeta.append(barra, leyenda);
      bloques.push(tarjeta);
      break;
    }
    case 'disco': {
      const d = r.disco;
      pintarCifras({ etiqueta: `${t('disk.read')} + ${t('disk.write')}`, valor: ritmo(d.lectura + d.escritura) }, [
        [t('disk.read'), ritmo(d.lectura)],
        [t('disk.write'), ritmo(d.escritura)],
        [t('disk.iops'), t('disk.iops_val', { r: f0.format(d.lecturas), w: f0.format(d.escrituras) })],
      ]);
      bloques.push(el('h3', 'section-header', t('disk.volumes')));
      const tarjeta = el('section', 'card lista');
      for (const v of d.volumenes) {
        const fila = el('div', 'fila');
        const izq = el('span', 'fila-texto');
        izq.append(el('span', 'fila-titulo', v.punto_montaje), el('span', 'fila-sub', `${v.sistema_archivos} · ${v.dispositivo}`));
        const der = el('span', 'volumen-derecha');
        der.append(
          el('span', 'fila-valor', t('disk.free', { free: bytes(v.disponible, 1000), total: bytes(v.total, 1000) })),
          medidor(1 - v.disponible / v.total),
        );
        fila.append(izq, der);
        tarjeta.append(fila);
      }
      bloques.push(tarjeta);
      break;
    }
    case 'red': {
      const n = r.red;
      pintarCifras({ etiqueta: t('net.down'), valor: ritmo(n.bajada) }, [
        [t('net.up'), ritmo(n.subida)],
        [t('net.total_down'), bytes(n.total_bajada, 1000)],
        [t('net.total_up'), bytes(n.total_subida, 1000)],
      ]);
      bloques.push(el('h3', 'section-header', t('net.interfaces')));
      const tarjeta = el('section', 'card lista');
      for (const i of n.interfaces) {
        const fila = el('div', 'fila');
        const izq = el('span', 'fila-texto');
        const titulo = el('span', 'fila-titulo', i.nombre);
        if (i.es_virtual) titulo.append(el('span', 'chip-virtual', t('net.virtual')));
        izq.append(titulo, el('span', 'fila-sub', [i.ip ?? t('net.no_ip'), i.mac].filter(Boolean).join(' · ')));
        fila.append(izq, el('span', 'fila-valor', `↓ ${ritmo(i.bajada)}   ↑ ${ritmo(i.subida)}`));
        tarjeta.append(fila);
      }
      bloques.push(tarjeta);
      break;
    }
    case 'bateria': {
      const b = r.bateria;
      if (!b) {
        sinDatos(t('bat.none'));
        break;
      }
      subtitulo = [b.fabricante, b.modelo, b.tecnologia].filter(Boolean).join(' · ');
      const lista = [[t('bat.state'), t(`st.${b.estado}`)]];
      // Con batería, la media de los últimos minutos acierta más que la
      // corriente instantánea del firmware, que salta con cada pico de carga.
      const prevision = r.energia.reciente?.minutos;
      if (b.estado === 'descargando' && prevision != null) {
        lista.push([t('bat.remaining'), `≈ ${duracion(prevision * 60)}`]);
      } else if (b.minutos_restantes != null) {
        lista.push([t(b.estado === 'cargando' ? 'bat.to_limit' : 'bat.remaining'), duracion(b.minutos_restantes * 60)]);
      }
      if (b.potencia_w != null) lista.push([t('bat.power'), `${f1.format(b.potencia_w)}\u00a0W`]);
      if (b.salud != null) lista.push([t('bat.health'), pct(b.salud, f0)]);
      if (b.limite_carga != null) lista.push([t('bat.limit'), pct(b.limite_carga, f0)]);
      lista.push([t('bat.adapter'), t(b.conectado ? 'bat.connected' : 'bat.disconnected')]);
      pintarCifras({ etiqueta: t('bat.level'), valor: pct(b.porcentaje, f0) }, lista);
      break;
    }
    case 'otros': {
      const temps = temperaturas(r.sensores);
      const caliente = masCaliente(r.sensores);
      if (!caliente && !r.sensores.chips.length) {
        sinDatos(t('sen.none'));
        break;
      }
      const ventiladores = [
        ...r.sensores.chips.flatMap((c) => c.ventiladores.map((v) => ({ nombre: v.etiqueta, rpm: v.valor }))),
        ...r.sensores.ventilacion,
      ];
      const avisos = temps.filter((l) => estadoSensor(l).clase);
      subtitulo = t('sen.subtitle', { chips: r.sensores.chips.length, n: temps.length });
      pintarCifras(
        caliente
          ? { etiqueta: t('sen.hottest'), valor: `${f1.format(caliente.valor)}\u00a0°C`, sub: caliente.etiqueta }
          : { etiqueta: t('sen.hottest'), valor: '—' },
        [
          [t('sen.average'), temps.length
            ? `${f1.format(temps.reduce((suma, l) => suma + l.valor, 0) / temps.length)}\u00a0°C` : '—'],
          [t('sen.fans'), ventiladores.length
            ? ventiladores.map(estadoVentilador).join(' · ') : t('sen.no_fans')],
          [t('sen.warnings'), avisos.length ? String(avisos.length) : t('sen.all_ok')],
        ],
      );
      for (const chip of r.sensores.chips) {
        const tarjeta = el('section', 'card lista');
        for (const l of chip.temperaturas) {
          const { clase, fraccion } = estadoSensor(l);
          const fila = el('div', 'fila');
          const izq = el('span', 'fila-texto');
          izq.append(el('span', 'fila-titulo', l.etiqueta));
          const limite = l.critico ?? l.maximo;
          if (limite) izq.append(el('span', 'fila-sub', t('sen.limit', { v: `${f0.format(limite)}\u00a0°C` })));
          const der = el('span', 'volumen-derecha');
          der.append(el('span', `fila-valor ${clase}`, `${f1.format(l.valor)}\u00a0°C`));
          if (fraccion != null) der.append(medidor(fraccion, clase || 'serie-1'));
          fila.append(izq, der);
          tarjeta.append(fila);
        }
        // Los ventiladores no se repiten aquí: van juntos en su tarjeta.
        const otras = [
          ...chip.tensiones.map((l) => [l.etiqueta, `${f2.format(l.valor)}\u00a0V`]),
          ...chip.corrientes.map((l) => [l.etiqueta, `${f2.format(l.valor)}\u00a0A`]),
          ...chip.potencias.map((l) => [l.etiqueta, `${f1.format(l.valor)}\u00a0W`]),
        ];
        for (const [etiqueta, valor] of otras) {
          const fila = el('div', 'fila');
          fila.append(el('span', 'fila-titulo', etiqueta), el('span', 'fila-valor', valor));
          tarjeta.append(fila);
        }
        bloques.push(el('h3', 'section-header', nombreChip(chip.nombre)), tarjeta);
      }
      if (ventiladores.length) {
        bloques.push(...conTitulo(t('otros.fans'), tarjetaDatos(
          ventiladores.map((v) => [v.nombre, estadoVentilador(v), v.rpm == null ? t('sen.no_rpm') : null]),
        )));
      }
      const plataforma = r.sensores.plataforma;
      if (plataforma) {
        bloques.push(...conTitulo(t('otros.platform'), tarjetaDatos([
          [t('otros.platform_mode'), nombreModo(plataforma.modo)],
          [t('otros.platform_options'), plataforma.opciones.map(nombreModo).join(' · ')],
          [t('otros.platform_driver'), plataforma.controlador],
        ])));
      }
      if (r.sensores.firmware.length) {
        bloques.push(...conTitulo(t('otros.firmware'), tarjetaDatos(
          r.sensores.firmware.map((a) => [a.nombre, valorAjuste(a)]),
        )));
      }
      break;
    }
  }
  poner($('#subtitulo'), subtitulo);
  extra.replaceChildren(...bloques);
  if (sec === 'bateria' && bat) actualizarBateria(r);

  const conProcesos = SECCIONES[sec].columnas !== null;
  $('#tabla').hidden = !conProcesos;
  $('#chips').hidden = !conProcesos;
  $('#lista-titulo').hidden = !conProcesos;
  btnFiltros.hidden = !conProcesos;
  if (!conProcesos) panelFiltros.hidden = true;
  filtrosActivos.hidden = !conProcesos || !estado.filtroGrupos.size;
  const nota = $('#nota');
  const notas = { red: 'net.note', bateria: 'bat.note', otros: 'sen.note' };
  nota.hidden = !notas[sec];
  if (notas[sec]) poner(nota, t(notas[sec]));
  dibujar();
}

// ── Bloques fijos: hardware y batería ─────────────────────────────────────
// No se rehacen en cada muestra como #extra: llevan controles, una gráfica con
// hover y datos que no cambian; rehacerlos cada 2 s quitaría foco y hover.
const fijo = $('#fijo');
let bat = null;

const PERFILES = { 'power-saver': 'profile.saver', balanced: 'profile.balanced', performance: 'profile.performance' };
const nombrePerfil = (p) => (PERFILES[p] ? t(PERFILES[p]) : p);
const MOTIVOS = { 'lap-detected': 'degraded.lap', 'high-operating-temperature': 'degraded.temp' };
const RANGOS = [[6, 'range.6h'], [24, 'range.24h'], [168, 'range.7d']];

function filaDato(titulo, valor, sub) {
  const fila = el('div', 'fila');
  const texto = el('span', 'fila-texto');
  texto.append(el('span', 'fila-titulo', titulo));
  if (sub) texto.append(el('span', 'fila-sub', sub));
  const dato = el('span', 'fila-valor dato', valor ?? '');
  fila.append(texto, dato);
  return { fila, dato };
}

// Filas [título, valor, subtítulo]. Las que no traen valor no se enseñan:
// cada equipo publica datos distintos y un «—» por fila sería ruido.
function tarjetaDatos(filas) {
  const visibles = filas.filter(([, valor]) => valor != null && valor !== '');
  if (!visibles.length) return null;
  const tarjeta = el('section', 'card lista');
  tarjeta.append(...visibles.map(([titulo, valor, sub]) => filaDato(titulo, valor, sub).fila));
  return tarjeta;
}

const conTitulo = (titulo, tarjeta) => (tarjeta ? [el('h3', 'section-header', titulo), tarjeta] : []);
const unirDatos = (...partes) => partes.filter(Boolean).join(' · ') || null;

function bloquesHardware(sec) {
  const h = estado.hardware;
  if (!h) return [];
  switch (sec) {
    case 'cpu': {
      const e = h.equipo;
      const c = h.cpu;
      const cache = c.cache
        .map((x) => `L${x.nivel}${{ Data: 'd', Instruction: 'i' }[x.tipo] ?? ''} ${bytes(x.bytes)}`)
        .join(' · ');
      return [
        ...conTitulo(t('hw.system'), tarjetaDatos([
          [t('hw.vendor'), e.fabricante],
          [t('hw.model'), e.modelo],
          [t('hw.board'), e.placa],
          [t('hw.bios'), e.bios],
          [t('hw.kernel'), e.nucleo],
        ])),
        ...conTitulo(t('hw.processor'), tarjetaDatos([
          [t('hw.vendor'), unirDatos(c.fabricante, c.arquitectura)],
          [t('cpu.cores'), t('hw.cores_val', { c: c.nucleos, h: c.hilos }),
            c.nucleos_rendimiento != null && c.nucleos_eficiencia != null
              ? t('hw.hybrid', { p: c.nucleos_rendimiento, e: c.nucleos_eficiencia }) : null],
          [t('hw.freq_range'), c.frecuencia_min_mhz != null && c.frecuencia_max_mhz != null
            ? `${f2.format(c.frecuencia_min_mhz / 1000)} – ${f2.format(c.frecuencia_max_mhz / 1000)} GHz` : null],
          [t('hw.cache'), cache, t('hw.cache_sub')],
          [t('hw.scaling'), unirDatos(c.controlador, c.gobernador)],
          [t('hw.microcode'), c.microcodigo],
          [t('hw.cpuid'), c.identificador],
        ])),
      ];
    }
    case 'gpu':
      return h.gpus.flatMap((g, i) => conTitulo(
        h.gpus.length > 1 ? `${t('hw.hardware')} ${i + 1}` : t('hw.hardware'),
        tarjetaDatos([
          [t('hw.model'), g.nombre],
          [t('hw.vendor'), g.fabricante],
          [t('hw.oem'), g.ensamblador],
          [t('gpu.driver'), g.controlador],
          [t('hw.pci'), g.pci],
          [t('hw.link'), g.enlace],
        ]),
      ));
    case 'ram': {
      const { modulos, maxima } = h.ram;
      if (!modulos.length) return [];
      const total = modulos.reduce((suma, m) => suma + m.bytes, 0);
      return conTitulo(t('hw.modules'), tarjetaDatos([
        [t('hw.installed'), bytes(total), maxima ? t('hw.max', { max: bytes(maxima) }) : null],
        // Con memoria soldada cada chip se publica como un módulo (este Galaxy
        // Book da 8 filas iguales de 4 GB): los idénticos van en una fila.
        ...Object.values(modulos.reduce((grupos, m) => {
          const clave = [m.bytes, m.tipo, m.velocidad_mts, m.fabricante, m.formato, m.rangos].join('|');
          grupos[clave] ??= { m, cuantos: 0 };
          grupos[clave].cuantos += 1;
          return grupos;
        }, {})).map(({ m, cuantos }) => [
          cuantos > 1 ? `${cuantos} × ${bytes(m.bytes)}` : (m.ubicacion ?? bytes(m.bytes)),
          unirDatos(m.tipo, m.velocidad_mts && `${f0.format(m.velocidad_mts)}\u00a0MT/s`),
          unirDatos(cuantos === 1 && bytes(m.bytes), m.fabricante, m.formato, m.rangos && t('hw.ranks', { n: m.rangos })),
        ]),
      ]));
    }
    case 'disco':
      return conTitulo(t('hw.drives'), tarjetaDatos(h.discos.map((d) => [
        d.modelo ?? d.dispositivo,
        bytes(d.bytes, 1000),
        unirDatos(d.conexion, d.dispositivo, d.rotacional ? 'HDD' : 'SSD', d.extraible && t('hw.removable'),
          d.firmware && `${t('hw.firmware')} ${d.firmware}`, d.serie && `${t('hw.serial')} ${d.serie}`),
      ])));
    case 'red':
      return conTitulo(t('hw.adapters'), tarjetaDatos(h.red.map((a) => [
        a.nombre ?? a.interfaz,
        t(`hw.net_${a.tipo}`),
        unirDatos(a.interfaz, a.controlador, a.bus),
      ])));
    case 'otros': {
      const o = h.otros;
      return [
        ...conTitulo(t('otros.screens'), tarjetaDatos(o.pantallas.map((p) => [
          p.conector,
          p.conectada ? (p.resolucion ?? t('otros.connected')) : t('otros.disconnected'),
        ]))),
        ...conTitulo(t('otros.cameras'), tarjetaDatos(o.camaras.map((c) => [
          c.nombre, c.cuantos > 1 ? t('otros.nodes', { n: c.cuantos }) : t('otros.node'),
        ]))),
        ...conTitulo(t('otros.sound'), tarjetaDatos(o.sonido.map((n) => [n, '']))),
        ...conTitulo(t('otros.bluetooth'), tarjetaDatos(o.bluetooth.map((n) => [n, '']))),
      ];
    }
    default:
      return [];
  }
}

function construirBateria() {
  const perfilesTitulo = el('h3', 'section-header', t('bat.modes'));
  const degradado = el('p', 'nota');
  degradado.hidden = true;
  const perfiles = el('section', 'card lista perfiles');
  perfiles.setAttribute('role', 'radiogroup');
  perfiles.setAttribute('aria-label', t('bat.modes'));

  const reciente = filaDato(t('bat.pred_recent'), '—', t('bat.pred_recent_sub'));
  const habitual = filaDato(t('bat.pred_usual'), '—', t('bat.pred_usual_sub'));
  const autonomiaTitulo = el('h3', 'section-header', t('bat.autonomy'));
  const autonomia = el('section', 'card lista');
  autonomia.append(reciente.fila, habitual.fila);

  const cabecera = el('div', 'lista-cabecera');
  const rangos = el('div', 'chips');
  rangos.setAttribute('role', 'tablist');
  for (const [horas, clave] of RANGOS) {
    const b = el('button', 'chip', t(clave));
    b.type = 'button';
    b.dataset.horas = String(horas);
    b.setAttribute('role', 'tab');
    b.setAttribute('aria-selected', String(horas === estado.rangoHistorial));
    rangos.append(b);
  }
  cabecera.append(el('h3', 'section-header', t('bat.history')), rangos);

  const historial = el('section', 'card bloque historial');
  const leyenda = el('div', 'leyenda');
  leyenda.append(
    el('span', 'clave serie-1', t('bat.level')),
    el('span', 'clave serie-2', t('bat.power')),
    el('span', 'clave-banda', t('st.cargando')),
  );
  const lienzoH = el('div', 'lienzo-historial');
  const canvasH = el('canvas');
  canvasH.setAttribute('role', 'img');
  const cruzH = el('div', 'cruz');
  cruzH.hidden = true;
  const tipH = el('div', 'tooltip');
  tipH.hidden = true;
  lienzoH.append(canvasH, cruzH, tipH);
  const vacioH = el('p', 'vacio', t('bat.no_history'));
  vacioH.hidden = true;
  historial.append(leyenda, lienzoH, vacioH);

  const hw = el('div');
  fijo.replaceChildren(perfilesTitulo, degradado, perfiles, autonomiaTitulo, autonomia, cabecera, historial, hw);

  bat = {
    perfilesTitulo, degradado, perfiles, reciente, habitual, autonomiaTitulo, autonomia, cabecera,
    historial, leyenda, lienzoH, canvasH, ctxH: canvasH.getContext('2d'), cruzH, tipH, vacioH, hw,
    clavePerfiles: '', geo: null, punteroX: null,
    observador: new ResizeObserver(() => dibujarHistorial()),
  };
  bat.observador.observe(lienzoH);

  perfiles.addEventListener('click', async (e) => {
    const fila = e.target.closest('.fila-perfil');
    if (!fila || fila.getAttribute('aria-checked') === 'true') return;
    const perfil = fila.dataset.perfil;
    // Se marca ya: el demonio tarda un instante y la siguiente muestra lo
    // confirma o, si falló, lo devuelve a su sitio.
    for (const f of perfiles.children) f.setAttribute('aria-checked', String(f === fila));
    try {
      await invoke('cambiar_perfil', { perfil });
      aviso(t('toast.profile', { name: nombrePerfil(perfil) }));
    } catch (clave) {
      aviso(t(String(clave)));
    }
    bucle();
  });
  rangos.addEventListener('click', (e) => {
    const b = e.target.closest('.chip');
    if (!b) return;
    estado.rangoHistorial = Number(b.dataset.horas);
    localStorage.setItem('monitor.historial', b.dataset.horas);
    for (const c of rangos.children) c.setAttribute('aria-selected', String(c === b));
    cargarHistorial();
  });
  lienzoH.addEventListener('pointermove', (e) => {
    bat.punteroX = e.clientX - lienzoH.getBoundingClientRect().left;
    tooltipHistorial();
  });
  lienzoH.addEventListener('pointerleave', () => {
    bat.punteroX = null;
    cruzH.hidden = true;
    tipH.hidden = true;
  });
}

function actualizarBateria(r) {
  const { perfiles: p, reciente, habitual, por_perfil: porPerfil } = r.energia;
  const b = r.bateria;
  bat.perfilesTitulo.hidden = !p;
  bat.perfiles.hidden = !p;
  bat.degradado.hidden = !p?.degradado;
  if (p) {
    const clave = p.disponibles.join('|');
    if (bat.clavePerfiles !== clave) {
      bat.clavePerfiles = clave;
      bat.perfiles.replaceChildren(...p.disponibles.map((id) => {
        const fila = el('button', 'fila fila-perfil');
        fila.type = 'button';
        fila.dataset.perfil = id;
        fila.setAttribute('role', 'radio');
        const texto = el('span', 'fila-texto');
        texto.append(el('span', 'fila-titulo', nombrePerfil(id)), el('span', 'fila-sub'));
        fila.append(el('span', 'opcion-marca'), texto);
        return fila;
      }));
    }
    for (const fila of bat.perfiles.children) {
      fila.setAttribute('aria-checked', String(fila.dataset.perfil === p.activo));
      const medida = porPerfil.find((x) => x.perfil === fila.dataset.perfil);
      poner(fila.querySelector('.fila-sub'), medida
        ? t('bat.profile_pred', {
          t: medida.minutos != null ? duracion(medida.minutos * 60) : '—',
          w: f1.format(medida.vatios),
          m: duracion(medida.medido_min * 60),
        })
        : t(b ? 'bat.profile_unmeasured' : 'bat.profile_no_battery'));
    }
    if (p.degradado) {
      poner(bat.degradado, t('bat.degraded', { motivo: MOTIVOS[p.degradado] ? t(MOTIVOS[p.degradado]) : p.degradado }));
    }
  }

  // Un sobremesa no tiene batería, pero sí modos de energía.
  for (const nodo of [bat.autonomiaTitulo, bat.autonomia, bat.cabecera, bat.historial, bat.hw]) nodo.hidden = !b;
  if (!b) return;
  const estimacion = (x) => {
    if (!x) return '—';
    return x.minutos != null
      ? t('bat.pred_val', { t: duracion(x.minutos * 60), w: f1.format(x.vatios) })
      : `${f1.format(x.vatios)} W`;
  };
  poner(bat.reciente.dato, estimacion(reciente));
  poner(bat.habitual.dato, estimacion(habitual));
  const wh = (v) => `${f1.format(v)} Wh`;
  bat.hw.replaceChildren(...conTitulo(t('hw.hardware'), tarjetaDatos([
    [t('hw.vendor'), b.fabricante],
    [t('hw.model'), b.modelo],
    [t('hw.serial_title'), b.serie],
    [t('hw.technology'), b.tecnologia],
    [t('bat.capacity'), b.capacidad_wh != null
      ? (b.capacidad_diseno_wh != null ? t('bat.capacity_val', { now: wh(b.capacidad_wh), design: wh(b.capacidad_diseno_wh) }) : wh(b.capacidad_wh))
      : null],
    [t('bat.cycles'), b.ciclos != null ? f0.format(b.ciclos) : null],
    [t('hw.voltage'), b.tension_v != null ? `${f2.format(b.tension_v)} V` : null,
      b.tension_diseno_v != null ? t('hw.nominal', { v: `${f2.format(b.tension_diseno_v)} V` }) : null],
    [t('hw.temperature'), b.temperatura_c != null ? `${f0.format(b.temperatura_c)} °C` : null],
  ])));
}

async function cargarHistorial() {
  const horas = estado.rangoHistorial;
  estado.historialPedido = Date.now();
  const historial = await invoke('historial_bateria', { horas });
  if (horas !== estado.rangoHistorial || estado.seccion !== 'bateria') return;
  estado.historial = historial;
  dibujarHistorial();
}

function dibujarHistorial() {
  if (!bat || !colores) return;
  const h = estado.historial;
  const hay = Boolean(h?.carga.some((v) => v != null));
  bat.lienzoH.hidden = !hay;
  bat.leyenda.hidden = !hay;
  bat.vacioH.hidden = hay || h === undefined;
  const ancho = bat.lienzoH.clientWidth;
  const alto = bat.lienzoH.clientHeight;
  if (!hay || !ancho) return;

  const c = bat.ctxH;
  const dpr = window.devicePixelRatio || 1;
  if (bat.canvasH.width !== Math.round(ancho * dpr) || bat.canvasH.height !== Math.round(alto * dpr)) {
    bat.canvasH.width = Math.round(ancho * dpr);
    bat.canvasH.height = Math.round(alto * dpr);
  }
  c.setTransform(dpr, 0, 0, dpr, 0, 0);
  c.clearRect(0, 0, ancho, alto);

  // Dos gráficas con el mismo eje de tiempo en vez de una con dos escalas:
  // porcentaje y vatios no se pueden leer contra el mismo eje.
  const x0 = 0;
  const x1 = ancho - 64;
  const n = h.carga.length;
  const paso = (x1 - x0) / n;
  const xDe = (i) => x0 + (i + 0.5) * paso;
  const paneles = [{ y0: 8, y1: 104 }, { y0: 136, y1: 232 }];
  const maxPotencia = escalaRedonda(Math.max(1, ...h.potencia.filter((v) => v != null)));

  c.fillStyle = colores.banda;
  h.cargando.forEach((cargando, i) => {
    if (cargando) c.fillRect(x0 + i * paso, paneles[0].y0, Math.ceil(paso), paneles[1].y1 - paneles[0].y0);
  });

  c.lineWidth = 1;
  c.strokeStyle = colores.rejilla;
  c.fillStyle = colores.texto;
  c.font = FUENTE_EJE;
  c.textBaseline = 'middle';
  const escalas = [
    { panel: paneles[0], maximo: 100, texto: (v) => pct(v, f0), valores: h.carga, color: colores.series[0] },
    { panel: paneles[1], maximo: maxPotencia, texto: (v) => `${f0.format(v)} W`, valores: h.potencia, color: colores.series[1] },
  ];
  for (const { panel, maximo, texto, valores, color } of escalas) {
    const yDe = (v) => panel.y1 - (Math.min(v, maximo) / maximo) * (panel.y1 - panel.y0);
    c.strokeStyle = colores.rejilla;
    c.fillStyle = colores.texto;
    for (const v of [0, maximo / 2, maximo]) {
      const y = Math.round(yDe(v)) + 0.5;
      c.beginPath();
      c.moveTo(x0, y);
      c.lineTo(x1, y);
      c.lineWidth = 1;
      c.stroke();
      c.fillText(texto(v), x1 + 8, y);
    }
    trazar(c, valores, xDe, yDe, panel.y1, color, false);
  }

  const formato = new Intl.DateTimeFormat(i18n.getLang(), estado.rangoHistorial > 24
    ? { weekday: 'short', hour: '2-digit', minute: '2-digit' }
    : { hour: '2-digit', minute: '2-digit' });
  c.fillStyle = colores.texto;
  c.textBaseline = 'alphabetic';
  for (const k of [0, 1, 2, 3, 4]) {
    const instante = h.desde + ((h.hasta - h.desde) * k) / 4;
    c.textAlign = k === 0 ? 'left' : k === 4 ? 'right' : 'center';
    c.fillText(formato.format(new Date(instante * 1000)), x0 + ((x1 - x0) * k) / 4, alto - 6);
  }
  c.textAlign = 'left';
  bat.geo = { x0, paso, n, xDe };
  bat.canvasH.setAttribute('aria-label', t('bat.history_label', { h: estado.rangoHistorial }));
  tooltipHistorial();
}

function tooltipHistorial() {
  const h = estado.historial;
  if (!bat || bat.punteroX == null || !bat.geo || !h) return;
  const { x0, paso, n, xDe } = bat.geo;
  const i = Math.min(n - 1, Math.max(0, Math.floor((bat.punteroX - x0) / paso)));
  const x = xDe(i);
  bat.cruzH.style.transform = `translateX(${Math.round(x)}px)`;
  bat.cruzH.hidden = false;

  const instante = h.desde + ((h.hasta - h.desde) * (i + 0.5)) / n;
  const cuando = el('div', 'tooltip-cuando', new Intl.DateTimeFormat(i18n.getLang(), {
    weekday: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  }).format(new Date(instante * 1000)));
  const fila = (clase, valor, nombre) => {
    const f = el('div', `tooltip-fila ${clase}`);
    f.append(el('strong', null, valor), el('span', null, nombre));
    return f;
  };
  const filas = [
    fila('clave serie-1', h.carga[i] == null ? '—' : pct(h.carga[i], f0), t('bat.level')),
    fila('clave serie-2', h.potencia[i] == null ? '—' : `${f1.format(h.potencia[i])} W`, t('bat.power')),
  ];
  if (h.cargando[i]) filas.push(fila('', '', t('st.cargando')));
  bat.tipH.replaceChildren(cuando, ...filas);
  bat.tipH.hidden = false;
  const anchoTip = bat.tipH.offsetWidth;
  const izquierda = x + 12 + anchoTip > bat.lienzoH.clientWidth ? x - 12 - anchoTip : x + 12;
  bat.tipH.style.transform = `translateX(${Math.round(izquierda)}px)`;
}

function pintarFijo() {
  if (estado.seccion === 'bateria') {
    if (!bat) construirBateria();
    if (estado.resumen) actualizarBateria(estado.resumen);
    dibujarHistorial();
    return;
  }
  bat?.observador.disconnect();
  bat = null;
  fijo.replaceChildren(...bloquesHardware(estado.seccion));
}

async function cargarHardware() {
  estado.hardware = await invoke('hardware');
  if (estado.seccion !== 'bateria') pintarFijo();
}

// ── Lista de procesos ─────────────────────────────────────────────────────
const cabeceraTabla = $('#tabla-cabecera');
const cuerpo = $('#tabla-cuerpo');
const filasDom = new Map();
const descriptores = new Map();

function ordenActual() {
  return estado.orden ?? { col: SECCIONES[estado.seccion].columnas?.[0] ?? 'nombre', desc: true };
}

function pintarCabeceraTabla() {
  const cols = SECCIONES[estado.seccion].columnas;
  if (!cols) return;
  $('#tabla').style.setProperty('--columnas', cols.length);
  const orden = ordenActual();
  const botones = ['nombre', ...cols].map((c) => {
    const b = el('button', 'col-h');
    b.type = 'button';
    b.dataset.col = c;
    b.setAttribute('role', 'columnheader');
    b.append(el('span', null, t(c === 'nombre' ? 'col.nombre' : COLUMNAS[c].titulo)));
    b.insertAdjacentHTML('beforeend', FLECHA_ORDEN);
    if (orden.col === c) b.setAttribute('aria-sort', orden.desc ? 'descending' : 'ascending');
    return b;
  });
  cabeceraTabla.replaceChildren(...botones);
}

cabeceraTabla.addEventListener('click', (e) => {
  const b = e.target.closest('.col-h');
  if (!b) return;
  const actual = ordenActual();
  const col = b.dataset.col;
  // Los nombres empiezan de la A; las cifras, de la mayor.
  estado.orden = actual.col === col ? { col, desc: !actual.desc } : { col, desc: col !== 'nombre' };
  pintarCabeceraTabla();
  pintarTabla();
});

function crearChips() {
  const chips = $('#chips');
  for (const f of FILTROS) {
    const b = el('button', 'chip');
    b.type = 'button';
    b.dataset.filtro = f;
    b.setAttribute('role', 'tab');
    b.addEventListener('click', () => {
      estado.filtro = f;
      // Volver a una pestaña de clase es dejar de mirar servicios sueltos.
      estado.filtroGrupos.clear();
      aplicarFiltros();
    });
    chips.append(b);
  }
}

function pintarChips() {
  for (const b of $('#chips').children) {
    poner(b, t(`filter.${b.dataset.filtro}`));
    const activa = b.dataset.filtro === estado.filtro && !estado.busqueda && !estado.filtroGrupos.size;
    b.setAttribute('aria-selected', String(activa));
  }
}

function construirFilas() {
  const cols = SECCIONES[estado.seccion].columnas;
  const orden = ordenActual();
  const q = estado.busqueda.trim().toLowerCase();
  const sentido = orden.desc ? -1 : 1;
  // «Con actividad» según lo que mide la sección: en Disco un proceso que
  // escribe sin gastar CPU está activo; en RAM la memoria no dice nada.
  const conActividad = {
    cpu: (p) => p.cpu >= 0.1,
    impacto: (p) => p.cpu >= 0.1,
    memoria: (p) => p.cpu >= 0.1,
    gpu: (p) => p.gpu >= 0.1,
    lectura: (p) => p.lectura + p.escritura > 0,
  }[cols[0]];
  const porGrupo = estado.filtroGrupos.size > 0;
  const comparar = (a, b) => (orden.col === 'nombre'
    ? a.nombre.localeCompare(b.nombre) * sentido
    : (COLUMNAS[orden.col].valor(a.valores) - COLUMNAS[orden.col].valor(b.valores)) * sentido
      || a.nombre.localeCompare(b.nombre));

  const items = [];
  for (const g of estado.grupos) {
    // Elegir servicios concretos manda sobre la pestaña de clase: si eliges
    // NetworkManager lo quieres ver aunque estés en Aplicaciones. Buscar texto
    // también busca en todas las clases.
    if (porGrupo) {
      if (!estado.filtroGrupos.has(g.id)) continue;
    } else if (!q && estado.filtro !== 'todo' && g.clase !== estado.filtro) {
      continue;
    }
    const coincideGrupo = !q || g.nombre.toLowerCase().includes(q) || idUnidad(g).includes(q);
    let procesos = coincideGrupo ? g.procesos
      : g.procesos.filter((p) => p.nombre.toLowerCase().includes(q) || String(p.pid) === q);
    if (estado.soloMios) procesos = procesos.filter((p) => p.propio);
    if (estado.soloActivos) procesos = procesos.filter(conActividad);
    if (!procesos.length) continue;
    const suma = { pid: null };
    for (const c of cols) {
      if (!COLUMNAS[c].sinSuma) suma[c] = 0;
    }
    for (const p of procesos) {
      suma.cpu = (suma.cpu ?? 0) + p.cpu;
      suma.hilos = (suma.hilos ?? 0) + p.hilos;
      suma.memoria = (suma.memoria ?? 0) + p.memoria;
      suma.lectura = (suma.lectura ?? 0) + p.lectura;
      suma.escritura = (suma.escritura ?? 0) + p.escritura;
      suma.gpu = (suma.gpu ?? 0) + p.gpu;
      suma.memoria_gpu = (suma.memoria_gpu ?? 0) + p.memoria_gpu;
    }
    const pids = new Set(procesos.map((p) => p.pid));
    const raices = procesos.filter((p) => p.padre == null || !pids.has(p.padre));
    // Si el grupo tiene un solo proceso raíz, el grupo lo representa: su PID
    // se muestra y «Finalizar» lo cierra a él y, con él, a la aplicación.
    const raiz = raices.reduce((a, b) => (b.pid < a.pid ? b : a));
    suma.pid = raices.length === 1 ? raiz.pid : null;
    items.push({
      clave: `g:${g.id}`,
      tipo: 'grupo',
      nivel: 0,
      nombre: g.nombre,
      icono: g.icono,
      clase: g.clase,
      valores: suma,
      pid: raiz.pid,
      contador: procesos.length,
      tieneHijos: true,
      procesos,
      forzado: Boolean(q) && !coincideGrupo,
    });
  }
  items.sort(comparar);

  const filas = [];
  for (const item of items) {
    item.abierto = item.forzado || estado.expandidos.has(item.clave);
    filas.push(item);
    if (!item.abierto) continue;
    const pids = new Set(item.procesos.map((p) => p.pid));
    const hijos = new Map();
    const raices = [];
    for (const p of item.procesos) {
      if (p.padre != null && pids.has(p.padre)) {
        if (!hijos.has(p.padre)) hijos.set(p.padre, []);
        hijos.get(p.padre).push(p);
      } else {
        raices.push(p);
      }
    }
    const aFila = (p, nivel) => ({
      clave: `p:${p.pid}`,
      tipo: 'proceso',
      nivel,
      nombre: p.nombre,
      valores: p,
      pid: p.pid,
      tieneHijos: hijos.has(p.pid) || p.hilos > 1,
    });
    const visitar = (p, nivel) => {
      const fila = aFila(p, nivel);
      fila.abierto = estado.expandidos.has(fila.clave);
      filas.push(fila);
      if (!fila.abierto) return;
      (hijos.get(p.pid) || []).map((h) => aFila(h, 0)).sort(comparar)
        .forEach((h) => visitar(h.valores, nivel + 1));
      for (const h of estado.hilos.get(p.pid) || []) {
        filas.push({ clave: `t:${h.tid}`, tipo: 'hilo', nivel: nivel + 1, nombre: h.nombre, hilo: h, tieneHijos: false });
      }
    };
    raices.map((p) => aFila(p, 0)).sort(comparar).forEach((f) => visitar(f.valores, 1));
  }
  return filas;
}

function crearFila() {
  const fila = el('div', 'fila-t');
  fila.setAttribute('role', 'row');
  fila.tabIndex = -1;
  const nombre = el('div', 'celda-nombre');
  nombre.setAttribute('role', 'gridcell');
  const chev = el('button', 'chev');
  chev.type = 'button';
  chev.tabIndex = -1;
  chev.innerHTML = CHEVRON;
  nombre.append(chev, el('span', 'icono-app'), el('span', 'nombre'), el('span', 'contador'));
  fila.append(nombre);
  return fila;
}

function actualizarFila(fila, f, cols) {
  fila.dataset.clave = f.clave;
  fila.className = `fila-t ${f.tipo}`;
  fila.style.setProperty('--nivel', f.nivel);
  fila.setAttribute('aria-level', String(f.nivel + 1));
  const seleccionada = estado.seleccion?.clave === f.clave;
  fila.setAttribute('aria-selected', String(seleccionada));

  const [chev, icono, nombre, contador] = fila.firstElementChild.children;
  chev.classList.toggle('sin-hijos', !f.tieneHijos);
  if (f.tieneHijos) {
    chev.setAttribute('aria-expanded', String(Boolean(f.abierto)));
    chev.setAttribute('aria-label', t('list.expand', { name: f.nombre }));
    fila.setAttribute('aria-expanded', String(Boolean(f.abierto)));
  } else {
    chev.removeAttribute('aria-expanded');
    fila.removeAttribute('aria-expanded');
  }

  if (f.tipo === 'grupo') {
    const clave = f.icono ?? f.clase;
    if (icono.dataset.clave !== clave) {
      icono.dataset.clave = clave;
      if (f.icono) {
        const img = new Image(22, 22);
        img.alt = '';
        img.decoding = 'async';
        img.src = convertFileSrc(f.icono);
        // Un icono que el ámbito del protocolo asset no deja leer cae al genérico.
        img.addEventListener('error', () => { icono.innerHTML = ICONO_APP; }, { once: true });
        icono.replaceChildren(img);
      } else {
        icono.innerHTML = f.clase === 'aplicacion' ? ICONO_APP : ICONO_SERVICIO;
      }
    }
    icono.hidden = false;
  } else {
    icono.hidden = true;
  }
  poner(nombre, f.nombre);
  poner(contador, f.tipo === 'grupo' && f.contador > 1 ? t('list.count', { n: f.contador }) : '');

  while (fila.children.length - 1 < cols.length) {
    const c = el('div', 'num');
    c.setAttribute('role', 'gridcell');
    fila.append(c);
  }
  while (fila.children.length - 1 > cols.length) fila.lastElementChild.remove();
  cols.forEach((c, i) => {
    let texto = '';
    if (f.tipo === 'hilo') {
      if (c === 'cpu' || c === 'impacto') texto = f1.format(f.hilo.cpu);
      if (c === 'pid') texto = String(f.hilo.tid);
    } else {
      const v = COLUMNAS[c].valor(f.valores);
      texto = v == null ? '' : COLUMNAS[c].texto(v);
    }
    poner(fila.children[i + 1], texto);
  });
}

function pintarTabla() {
  const cols = SECCIONES[estado.seccion].columnas;
  if (!cols) return;
  const filas = construirFilas();
  const conFoco = document.activeElement?.closest?.('.fila-t');
  const vistas = new Set();
  let anterior = null;
  descriptores.clear();
  for (const f of filas) {
    let fila = filasDom.get(f.clave);
    if (!fila) {
      fila = crearFila();
      filasDom.set(f.clave, fila);
    }
    actualizarFila(fila, f, cols);
    descriptores.set(f.clave, f);
    vistas.add(f.clave);
    const esperado = anterior ? anterior.nextSibling : cuerpo.firstChild;
    if (fila !== esperado) cuerpo.insertBefore(fila, esperado);
    anterior = fila;
  }
  for (const [clave, fila] of filasDom) {
    if (!vistas.has(clave)) {
      fila.remove();
      filasDom.delete(clave);
    }
  }
  // Mover un nodo en el DOM le quita el foco; se le devuelve sin desplazar.
  if (conFoco && conFoco.isConnected && document.activeElement !== conFoco) conFoco.focus({ preventScroll: true });

  // Solo una fila entra en el orden de tabulación; las flechas mueven dentro.
  const enfocable = (estado.seleccion && filasDom.get(estado.seleccion.clave)) || cuerpo.firstElementChild;
  for (const fila of cuerpo.children) fila.tabIndex = fila === enfocable ? 0 : -1;

  const vacia = $('#tabla-vacia');
  vacia.hidden = filas.length > 0;
  if (!filas.length) poner(vacia, estado.grupos.length ? t('list.empty') : t('list.loading'));
  $('#btn-finalizar').hidden = !estado.seleccion;
}

async function alternar(f) {
  if (!f.tieneHijos || f.forzado) return;
  if (estado.expandidos.has(f.clave)) {
    estado.expandidos.delete(f.clave);
    if (f.tipo === 'proceso') estado.hilos.delete(f.pid);
  } else {
    estado.expandidos.add(f.clave);
    if (f.tipo === 'proceso') estado.hilos.set(f.pid, await invoke('hilos', { pid: f.pid }));
  }
  pintarTabla();
}

function seleccionar(f) {
  if (f.tipo === 'hilo') return;
  estado.seleccion = { clave: f.clave, pid: f.pid, nombre: f.nombre };
  pintarTabla();
  filasDom.get(f.clave)?.focus({ preventScroll: true });
}

cuerpo.addEventListener('click', (e) => {
  const fila = e.target.closest('.fila-t');
  const f = fila && descriptores.get(fila.dataset.clave);
  if (!f) return;
  if (e.target.closest('.chev')) alternar(f);
  else seleccionar(f);
});
cuerpo.addEventListener('dblclick', (e) => {
  const fila = e.target.closest('.fila-t');
  const f = fila && descriptores.get(fila.dataset.clave);
  if (f && !e.target.closest('.chev')) alternar(f);
});
cuerpo.addEventListener('keydown', (e) => {
  const fila = e.target.closest('.fila-t');
  const f = fila && descriptores.get(fila.dataset.clave);
  if (!f) return;
  const mover = (destino) => {
    if (!destino) return;
    const g = descriptores.get(destino.dataset.clave);
    if (g) seleccionar(g);
    destino.focus();
    destino.scrollIntoView({ block: 'nearest' });
  };
  switch (e.key) {
    case 'ArrowDown': mover(fila.nextElementSibling); break;
    case 'ArrowUp': mover(fila.previousElementSibling); break;
    case 'ArrowRight': if (f.tieneHijos && !f.abierto) alternar(f); break;
    case 'ArrowLeft': if (f.abierto) alternar(f); break;
    case 'Enter': alternar(f); break;
    case 'Delete': seleccionar(f); abrirDialogo(); break;
    default: return;
  }
  e.preventDefault();
});

// ── Filtros ───────────────────────────────────────────────────────────────
const btnFiltros = $('#btn-filtros');
const panelFiltros = $('#panel-filtros');
const listaFiltros = $('#filtros-lista');
const buscarServicio = $('#filtros-buscar');
const filtrosActivos = $('#filtros-activos');
const ORDEN_CLASES = ['aplicacion', 'sesion', 'segundo_plano', 'sistema'];

// El id llega como «Sistema:NetworkManager»; el prefijo de clase no se busca,
// o escribir «sistema» casaría con todos los servicios del sistema.
const idUnidad = (g) => g.id.slice(g.id.indexOf(':') + 1).toLowerCase();

function aplicarFiltros() {
  pintarFiltros();
  pintarChips();
  pintarTabla();
}

// Solo se repinta al cambiar un filtro, no en cada muestra: rehacer los
// botones cada 2 s quitaría el foco y el hover a quien los está usando.
function pintarFiltros() {
  const n = estado.filtroGrupos.size + Number(estado.soloActivos) + Number(estado.soloMios);
  poner(btnFiltros.querySelector('span'), n ? t('filters.button_n', { n }) : t('filters.button'));
  btnFiltros.setAttribute('aria-expanded', String(!panelFiltros.hidden));
  $('#filtro-activos').setAttribute('aria-checked', String(estado.soloActivos));
  $('#filtro-mios').setAttribute('aria-checked', String(estado.soloMios));
  $('#filtros-pie').hidden = n === 0;

  const nombres = new Map(estado.grupos.map((g) => [g.id, g.nombre]));
  filtrosActivos.replaceChildren(...[...estado.filtroGrupos].map((id) => {
    const nombre = nombres.get(id) ?? id.slice(id.indexOf(':') + 1);
    const b = el('button', 'chip chip-filtro');
    b.type = 'button';
    b.dataset.id = id;
    b.setAttribute('aria-label', t('filters.remove', { name: nombre }));
    b.append(el('span', null, nombre));
    b.insertAdjacentHTML('beforeend', ICONO_QUITAR);
    return b;
  }));
  filtrosActivos.hidden = !estado.filtroGrupos.size || SECCIONES[estado.seccion].columnas === null;
}

function pintarListaFiltros() {
  const q = buscarServicio.value.trim().toLowerCase();
  const grupos = estado.grupos
    .filter((g) => !q || g.nombre.toLowerCase().includes(q) || idUnidad(g).includes(q))
    .sort((a, b) => ORDEN_CLASES.indexOf(a.clase) - ORDEN_CLASES.indexOf(b.clase)
      || a.nombre.localeCompare(b.nombre));
  const opciones = grupos.map((g) => {
    const b = el('button', 'opcion');
    b.type = 'button';
    b.dataset.id = g.id;
    b.setAttribute('role', 'checkbox');
    b.setAttribute('aria-checked', String(estado.filtroGrupos.has(g.id)));
    b.append(el('span', 'opcion-marca'), el('span', 'opcion-texto', g.nombre), el('span', 'opcion-clase', t(`clase.${g.clase}`)));
    return b;
  });
  listaFiltros.replaceChildren(...(opciones.length ? opciones : [el('p', 'opcion-vacia', t('filters.none'))]));
}

function cerrarPanelFiltros() {
  panelFiltros.hidden = true;
  pintarFiltros();
  btnFiltros.focus({ preventScroll: true });
}

btnFiltros.addEventListener('click', () => {
  if (!panelFiltros.hidden) {
    cerrarPanelFiltros();
    return;
  }
  panelFiltros.hidden = false;
  buscarServicio.value = '';
  pintarListaFiltros();
  pintarFiltros();
  buscarServicio.focus({ preventScroll: true });
});
buscarServicio.addEventListener('input', pintarListaFiltros);
panelFiltros.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') cerrarPanelFiltros();
});
listaFiltros.addEventListener('click', (e) => {
  const b = e.target.closest('.opcion');
  if (!b) return;
  const id = b.dataset.id;
  if (estado.filtroGrupos.has(id)) estado.filtroGrupos.delete(id);
  else estado.filtroGrupos.add(id);
  b.setAttribute('aria-checked', String(estado.filtroGrupos.has(id)));
  aplicarFiltros();
});
filtrosActivos.addEventListener('click', (e) => {
  const b = e.target.closest('.chip-filtro');
  if (!b) return;
  estado.filtroGrupos.delete(b.dataset.id);
  if (!panelFiltros.hidden) pintarListaFiltros();
  aplicarFiltros();
});
$('#filtro-activos').addEventListener('click', () => {
  estado.soloActivos = !estado.soloActivos;
  aplicarFiltros();
});
$('#filtro-mios').addEventListener('click', () => {
  estado.soloMios = !estado.soloMios;
  aplicarFiltros();
});
$('#filtros-quitar').addEventListener('click', () => {
  estado.filtroGrupos.clear();
  estado.soloActivos = false;
  estado.soloMios = false;
  pintarListaFiltros();
  aplicarFiltros();
});

// ── Finalizar proceso ─────────────────────────────────────────────────────
const overlay = $('#overlay');
const forzar = $('#dialogo-forzar');
let focoPrevio = null;

function abrirDialogo() {
  const s = estado.seleccion;
  if (!s) return;
  poner($('#dialogo-titulo'), t('dialog.end_title', { name: s.nombre }));
  poner($('#dialogo-msg'), t('dialog.end_msg', { pid: s.pid }));
  forzar.setAttribute('aria-checked', 'false');
  focoPrevio = document.activeElement;
  overlay.hidden = false;
  $('#dialogo-cancelar').focus();
}
function cerrarDialogo() {
  overlay.hidden = true;
  focoPrevio?.focus?.({ preventScroll: true });
}

$('#btn-finalizar').addEventListener('click', abrirDialogo);
$('#dialogo-cancelar').addEventListener('click', cerrarDialogo);
overlay.addEventListener('click', (e) => { if (e.target === overlay) cerrarDialogo(); });
overlay.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    cerrarDialogo();
    return;
  }
  if (e.key !== 'Tab') return;
  const enfocables = [...overlay.querySelectorAll('button')];
  const i = enfocables.indexOf(document.activeElement);
  const siguiente = (i + (e.shiftKey ? -1 : 1) + enfocables.length) % enfocables.length;
  enfocables[siguiente].focus();
  e.preventDefault();
});
$('#dialogo-aceptar').addEventListener('click', async () => {
  const s = estado.seleccion;
  const forzado = forzar.getAttribute('aria-checked') === 'true';
  cerrarDialogo();
  try {
    await invoke('finalizar', { pid: s.pid, forzar: forzado });
    aviso(t('toast.ended', { name: s.nombre }));
    estado.seleccion = null;
    await cargarProcesos();
  } catch (clave) {
    aviso(t(String(clave)));
  }
});

for (const toggle of document.querySelectorAll('.toggle')) {
  toggle.addEventListener('click', () => {
    toggle.setAttribute('aria-checked', String(toggle.getAttribute('aria-checked') !== 'true'));
  });
}

let temporizadorAviso = 0;
function aviso(mensaje) {
  const toast = $('#toast');
  toast.hidden = true;
  toast.textContent = mensaje;
  // Forzar un reflow reinicia la animación de entrada si ya había un aviso.
  void toast.offsetWidth;
  toast.hidden = false;
  clearTimeout(temporizadorAviso);
  temporizadorAviso = setTimeout(() => { toast.hidden = true; }, 3000);
}

// ── Ajustes ───────────────────────────────────────────────────────────────
async function aplicarTema() {
  const raiz = document.documentElement;
  const tema = await invoke('tema_sistema');
  raiz.classList.toggle('dark-mode', tema === 'dark');
  raiz.classList.toggle('light-mode', tema === 'light');
  leerColores();
  dibujar();
  dibujarHistorial();
}

// Nombres que i18n.js no pone sin dejar además un tooltip (ver index.html).
function etiquetasSinTooltip() {
  document.title = t('app.title');
  $('#buscar').setAttribute('aria-label', t('search.ph'));
  buscarServicio.setAttribute('aria-label', t('filters.search_ph'));
}

function conectarAjustes() {
  const intervalo = $('#ajuste-intervalo');
  intervalo.value = String(estado.intervalo);
  intervalo.addEventListener('change', () => {
    estado.intervalo = Number(intervalo.value);
    localStorage.setItem('monitor.intervalo', intervalo.value);
    // El eje de tiempo se calcula con el intervalo actual: mezclar muestras
    // de dos ritmos distintos pondría mal los segundos.
    estado.historia = nuevaHistoria();
    bucle();
  });

  const paleta = $('#ajuste-paleta');
  paleta.setAttribute('aria-checked', String(window.bookosPalette.enabled()));
  paleta.addEventListener('click', () => {
    window.bookosPalette.setEnabled(paleta.getAttribute('aria-checked') === 'true');
  });
  window.addEventListener('bookos-palette-changed', () => {
    leerColores();
    dibujar();
    dibujarHistorial();
  });

  const buscar = $('#buscar');
  buscar.addEventListener('input', () => {
    estado.busqueda = buscar.value;
    if (buscar.value && SECCIONES[estado.seccion].columnas === null) cambiarSeccion('cpu');
    pintarChips();
    pintarTabla();
  });
  buscar.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && buscar.value) {
      buscar.value = '';
      buscar.dispatchEvent(new Event('input'));
    }
  });

  // El tema del escritorio puede cambiar con la app abierta: se vuelve a
  // preguntar al recuperar el foco, igual que hace la paleta dinámica.
  window.addEventListener('focus', aplicarTema);

  $('#btn-min').addEventListener('click', () => ventanaTauri.minimize());
  $('#btn-max').addEventListener('click', () => ventanaTauri.toggleMaximize());
  $('#btn-cerrar').addEventListener('click', () => ventanaTauri.close());
  ventanaTauri.onResized(async () => {
    $('#ventana').classList.toggle('maximizada', await ventanaTauri.isMaximized());
  });
}

// ── Muestreo ──────────────────────────────────────────────────────────────
function cambiarSeccion(id) {
  const cambia = id !== estado.seccion;
  estado.seccion = id;
  localStorage.setItem('monitor.seccion', id);
  if (cambia) {
    bat?.observador.disconnect();
    bat = null;
    estado.historial = undefined;
    estado.orden = null;
    estado.grupos = [];
    for (const fila of filasDom.values()) fila.remove();
    filasDom.clear();
    const detalle = $('#detalle');
    detalle.scrollTop = 0;
    detalle.classList.remove('pagina-entra');
    void detalle.offsetWidth;
    detalle.classList.add('pagina-entra');
  }
  pintarNav();
  pintarCabeceraTabla();
  pintarPagina();
  pintarTabla();
  pintarFijo();
  if (cambia && SECCIONES[id].columnas) cargarProcesos();
  if (cambia) {
    // Se vuelve a leer al entrar: un disco o un adaptador USB pueden haberse
    // enchufado desde la última vez.
    cargarHardware();
    if (id === 'bateria') cargarHistorial();
  }
}

async function cargarProcesos() {
  const seccion = estado.seccion;
  const grupos = await invoke('procesos', { seccion });
  if (seccion !== estado.seccion) return;
  estado.grupos = grupos;

  const vivos = new Set(grupos.flatMap((g) => g.procesos.map((p) => p.pid)));
  for (const clave of estado.expandidos) {
    if (clave.startsWith('p:') && !vivos.has(Number(clave.slice(2)))) estado.expandidos.delete(clave);
  }
  if (estado.seleccion && !vivos.has(estado.seleccion.pid)) estado.seleccion = null;
  const abiertos = [...estado.expandidos].filter((c) => c.startsWith('p:')).map((c) => Number(c.slice(2)));
  estado.hilos = new Map(await Promise.all(abiertos.map(async (pid) => [pid, await invoke('hilos', { pid })])));
  pintarTabla();
}

async function muestra() {
  const conProcesos = SECCIONES[estado.seccion].columnas !== null;
  const [r] = await Promise.all([invoke('resumen'), conProcesos ? cargarProcesos() : null]);
  registrar(r);
  estado.resumen = r;
  pintarNav();
  pintarPagina();
  // UPower vuelca su historial cada pocos minutos: pedirlo más a menudo no trae nada.
  if (estado.seccion === 'bateria' && Date.now() - estado.historialPedido > 60_000) cargarHistorial();
}

let temporizador = 0;
let ocupado = false;
// Una muestra tras otra con setTimeout, no setInterval: si una lectura tarda
// más que el intervalo no se amontonan. Oculta o minimizada no se lee nada.
async function bucle() {
  clearTimeout(temporizador);
  if (ocupado) return;
  ocupado = true;
  try {
    if (!document.hidden && !(await ventanaTauri.isMinimized())) await muestra();
  } finally {
    ocupado = false;
    if (!document.hidden) temporizador = setTimeout(bucle, estado.intervalo);
  }
}
document.addEventListener('visibilitychange', () => {
  if (document.hidden) clearTimeout(temporizador);
  else bucle();
});

async function iniciar() {
  // locales.js solo trae español e inglés; cualquier otro idioma cae en español.
  const idioma = await invoke('idioma_sistema');
  i18n.setLang(idioma.toLowerCase().startsWith('en') ? 'en' : 'es');
  prepararFormatos();
  etiquetasSinTooltip();
  crearNav();
  crearChips();
  conectarAjustes();
  await aplicarTema();
  cambiarSeccion(estado.seccion);
  cargarHardware();
  if (estado.seccion === 'bateria') cargarHistorial();
  pintarChips();
  // La primera lectura solo fija los contadores de referencia: sus
  // porcentajes serían la media desde el arranque del equipo.
  await Promise.all([invoke('resumen'), invoke('procesos', { seccion: estado.seccion })]);
  await ventanaTauri.show();
  temporizador = setTimeout(bucle, 500);
}

// Si arrancar falla, la ventana se enseña igual con el aviso a la vista: una
// app corriendo sin ventana no hay forma de cerrarla desde el escritorio.
iniciar().catch(async (error) => {
  try {
    await ventanaTauri.show();
  } catch {}
  aviso(String(error?.message ?? error));
  console.error("fallo al arrancar el monitor", error);
});
