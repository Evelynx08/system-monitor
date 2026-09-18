// Shared i18n engine for BookOS apps.
// Define BOOKOS_I18N dict before loading this script (or after, doesn't matter,
// as long as applyI18n() is called once it's set).
(function(){
  let CUR = "es";
  function detect() {
    const s = (navigator.language || "es").toLowerCase();
    return s.startsWith("en") ? "en" : "es";
  }
  function dict() { return (window.BOOKOS_I18N && window.BOOKOS_I18N[CUR]) || (window.BOOKOS_I18N && window.BOOKOS_I18N.es) || {}; }
  function t(key, vars) {
    let s = dict()[key];
    if (s == null && window.BOOKOS_I18N && window.BOOKOS_I18N.es) s = window.BOOKOS_I18N.es[key];
    if (s == null) s = key;
    if (vars) for (const k in vars) s = s.split("{"+k+"}").join(vars[k]);
    return s;
  }
  function setLang(l) {
    CUR = (l === "auto" || !l) ? detect() : l;
    document.documentElement.lang = CUR;
    apply();
  }
  function getLang() { return CUR; }
  function apply() {
    document.querySelectorAll("[data-i18n]").forEach(el => { el.textContent = t(el.dataset.i18n); });
    document.querySelectorAll("[data-i18n-html]").forEach(el => { el.innerHTML = t(el.dataset.i18nHtml); });
    document.querySelectorAll("[data-i18n-title]").forEach(el => { el.title = t(el.dataset.i18nTitle); el.setAttribute("aria-label", t(el.dataset.i18nTitle)); });
    document.querySelectorAll("[data-i18n-ph]").forEach(el => { el.placeholder = t(el.dataset.i18nPh); });
    const titleKey = document.querySelector("body")?.dataset?.i18nTitle;
    if (titleKey) document.title = t(titleKey);
  }
  window.BookosI18n = { t, setLang, getLang, apply, detect };
})();
