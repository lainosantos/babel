'use strict';

// Catalogs contain plain text only. Keys are stable; parameters never become HTML.
// The server resolves the operating-system locale. Browser language is not used.
(() => {
  const catalogs = new Map();
  const requests = new Map();
  const rendered = new Map();
  const messages = new Map();
  let language = 'en';
  async function load(code) {
    if (catalogs.has(code)) return;
    if (!/^[a-z]{2,3}(?:-[A-Za-z0-9]{2,8})*$/.test(code)) throw new Error('Invalid interface locale');
    if (!requests.has(code)) requests.set(code, (async () => {
      const response = await fetch(`/locales/${code}.json`, { cache: 'no-cache', credentials: 'omit' });
      if (!response.ok) throw new Error(`Could not load interface catalog (${response.status})`);
      const values = await response.json();
      if (!values || Array.isArray(values) || typeof values !== 'object') throw new Error('Invalid interface catalog');
      catalogs.set(code, values);
    })().finally(() => requests.delete(code)));
    await requests.get(code);
  }
  function t(key, values = {}) {
    const selected = catalogs.get(language)?.[key];
    const fallback = catalogs.get('en')?.[key];
    const template = typeof selected === 'string' ? selected : typeof fallback === 'string' ? fallback : key;
    const result = template.replace(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g, (match, parameter) => Object.hasOwn(values, parameter) ? String(values[parameter]) : match);
    rendered.set(result, { key, values });
    if (rendered.size > 1024) rendered.delete(rendered.keys().next().value);
    return result;
  }
  function apply(root = document) {
    for (const element of root.querySelectorAll('[data-i18n]')) element.textContent = t(element.dataset.i18n);
    for (const attribute of ['aria-label', 'placeholder', 'title']) {
      for (const element of root.querySelectorAll(`[data-i18n-${attribute}]`)) element.setAttribute(attribute, t(element.getAttribute(`data-i18n-${attribute}`)));
    }
    for (const [element, message] of messages) {
      if (!element.isConnected) { messages.delete(element); continue; }
      element.textContent = message.key ? t(message.key, message.values) : message.raw;
    }
    document.documentElement.lang = language;
  }
  function message(element, value) {
    messages.set(element, rendered.get(value) || { raw: value });
    element.textContent = value;
  }
  window.BabelI18n = Object.freeze({
    load, t, apply, message,
    async setLanguage(code) { await load('en'); await load(code); language = code; apply(); window.dispatchEvent(new CustomEvent('babel:languagechange')); },
    get language() { return language; },
    number(value, options) { return new Intl.NumberFormat(language, options).format(value); },
    date(value, options) { return new Intl.DateTimeFormat(language, options).format(value); },
    plural(value) { return new Intl.PluralRules(language).select(value); },
  });
})();
