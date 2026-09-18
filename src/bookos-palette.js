// Cliente del color dinámico de BookOS — compartido por todas las apps.
//
// Ajustes genera la paleta desde el fondo de pantalla y la deja en
// ~/.config/bookos/palette.css. Este script la mete en un <style> al final de
// <head> cuando la app lo permite. Si el usuario desactiva el color dinámico,
// la hoja se retira y la app continúa con sus valores locales.
//
// Se carga como script normal (no módulo) desde el <head>, DESPUÉS del
// <link rel="stylesheet">:
//     <script src="bookos-palette.js"></script>
//
// Requiere en el backend de la app un único comando:
//     #[tauri::command] fn bookos_palette_css() -> String
//
// Fichero idéntico en todas las apps. Si se cambia aquí, cópiese tal cual;
// el original vive en BookOS-Settings/src-tauri/extra/dynamic-color/.

(function () {
    'use strict';
    var STYLE_ID = 'bookos-dynamic-palette';
    var ALIAS_STYLE_ID = 'bookos-dynamic-palette-aliases';
    var CACHE_KEY = '__bookos_palette_css__';
    var ENABLED_KEY = 'bookos.dynamicPalette';
    var invokeFn = null;

    function enabled() {
        try { return localStorage.getItem(ENABLED_KEY) !== 'false'; }
        catch (e) { return true; }
    }

    function install(css) {
        var el = document.getElementById(STYLE_ID);
        var aliases = document.getElementById(ALIAS_STYLE_ID);
        if (!css) {
            if (el) el.remove();
            if (aliases) aliases.remove();
            try { localStorage.removeItem(CACHE_KEY); } catch (e) {}
            window.dispatchEvent(new CustomEvent('bookos-palette-changed'));
            return;
        }
        if (!el) { el = document.createElement('style'); el.id = STYLE_ID; }
        el.textContent = css;
        // Reinsertar al final aunque ya existiera: cualquier hoja añadida
        // después quedaría por encima y ganaría el desempate de la cascada.
        (document.head || document.documentElement).appendChild(el);
        if (!aliases) {
            aliases = document.createElement('style');
            aliases.id = ALIAS_STYLE_ID;
        }
        // El contrato dinámico usa los nombres cortos de Settings. El Shell
        // conserva sus nombres históricos mediante alias, sin copiar colores
        // ni depender de que exista la hoja compartida.
        aliases.textContent = 'html[data-theme="light"],html[data-theme="dark"]{' +
            '--bg-window:var(--bg);--bg-elevated:var(--card);--bg-alt:var(--sbg);' +
            '--bg-term:var(--bg);--bg-glass:color-mix(in srgb,var(--card) 72%,transparent);' +
            '--fg:var(--tx);--fg-dim:var(--tx2);--border:var(--brd);' +
            '--accent:var(--blue);--accent-hover:var(--blue);' +
            '--selection:color-mix(in srgb,var(--blue) 25%,transparent)}';
        (document.head || document.documentElement).appendChild(aliases);
        try { localStorage.setItem(CACHE_KEY, css); } catch (e) {}
        window.dispatchEvent(new CustomEvent('bookos-palette-changed'));
    }

    // Primero la copia en caché, de forma síncrona: esperar al backend haría
    // que la ventana abriese con los colores de fábrica y saltase al color del
    // fondo medio segundo después, que se ve peor que no teñir nada.
    try {
        var cached = localStorage.getItem(CACHE_KEY);
        if (enabled() && cached) install(cached);
        if (!enabled()) install('');
    } catch (e) {}

    function sync() {
        if (!enabled()) {
            install('');
            return;
        }
        var inv = invokeFn || (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
        if (!inv) return;
        inv('bookos_palette_css')
            .then(function (css) { install(css || ''); })
            .catch(function () {});
    }

    // Al acabar de analizar el HTML se vuelve a colocar la hoja al final. El
    // script corre en mitad del <head>, así que cualquier <link> que venga
    // DESPUÉS quedaría por detrás en el DOM y ganaría el desempate; reinsertar
    // aquí la devuelve al último lugar, ya con todas las hojas cargadas.
    function reseat() {
        if (!enabled()) {
            install('');
            return;
        }
        var el = document.getElementById(STYLE_ID);
        if (el) document.head.appendChild(el);
        var aliases = document.getElementById(ALIAS_STYLE_ID);
        if (aliases) document.head.appendChild(aliases);
        sync();
    }
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', reseat);
    } else {
        reseat();
    }
    // La paleta puede cambiar mientras la app está abierta (el usuario toca
    // otro color en Ajustes, o cambia el fondo). Volver a leerla al recuperar
    // el foco cuesta una IPC y evita tener que montar un vigilante de ficheros
    // en siete aplicaciones.
    window.addEventListener('focus', sync);
    window.bookosPalette = {
        enabled: enabled,
        setEnabled: function (value) {
            try { localStorage.setItem(ENABLED_KEY, value ? 'true' : 'false'); } catch (e) {}
            if (value) sync();
            else install('');
        },
        setInvoker: function (fn) {
            invokeFn = typeof fn === 'function' ? fn : null;
            if (invokeFn) sync();
        },
        sync: sync
    };
})();
