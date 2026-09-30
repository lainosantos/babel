'use strict';

// Workspace views keep the same controls, drafts and live audio session.
// They change visibility only; the backend never participates in navigation.
(() => {
  const views = ['routing', 'translation', 'transcription', 'recording', 'commands', 'settings'];
  const aliases = { audio: 'routing', voices: 'translation', files: 'recording' };
  const viewName = view => Object.hasOwn(aliases, view) ? aliases[view] : view;
  const english = {
    routing: ['Audio routing', 'Choose the real and virtual devices for each direction.'],
    translation: ['Translation', 'Set languages and providers for what you say and hear.'],
    transcription: ['Transcription', 'Keep the original words from both sides of the conversation.'],
    recording: ['Recording', 'Save the original audio from both sides in one recording.'],
    commands: ['Voice commands', 'Use your microphone to control connected tools.'],
    settings: ['Settings', 'Configure storage, system settings and startup on this computer.'],
  };
  const storageKey = 'babel-workspace-view';
  let current = 'routing';
  let nativeReport = false;
  let firstInvalid = null;
  let validation = null;
  const byId = id => document.getElementById(id);
  const t = (key, values) => window.BabelI18n?.t(key, values) || key;
  const panes = () => [...document.querySelectorAll('[data-workspace-panel]')];
  const navigation = () => [...document.querySelectorAll('.nav-item[data-workspace-target]')];

  function renderHeader() {
    for (const [id, suffix, fallbackIndex] of [['page-title', 'title', 0], ['page-description', 'description', 1]]) {
      const node = byId(id);
      if (!node) continue;
      const key = `workspace.${current}_${suffix}`;
      node.dataset.i18n = key;
      const label = t(key);
      node.textContent = label === key ? english[current][fallbackIndex] : label;
    }
  }
  function saveView() { try { sessionStorage.setItem(storageKey, current); } catch (_) { /* Navigation still works with storage disabled. */ } }
  function navigate(view, options = {}) {
    view = viewName(view);
    if (!views.includes(view)) return false;
    const changed = current !== view;
    current = view;
    for (const pane of panes()) pane.hidden = viewName(pane.dataset.workspacePanel) !== current;
    for (const button of navigation()) {
      const active = viewName(button.dataset.workspaceTarget) === current;
      button.classList.toggle('active', active);
      if (active) button.setAttribute('aria-current', 'page'); else button.removeAttribute('aria-current');
    }
    document.documentElement.dataset.workspaceView = current;
    renderHeader();
    if (options.remember !== false) saveView();
    // A shortcut inside the old panel should not leave keyboard focus hidden.
    // Open dialogs and navigation buttons retain their existing focus.
    const focusHidden = document.activeElement?.closest('[data-workspace-panel][hidden]');
    if (options.focus || focusHidden) {
      const title = byId('page-title');
      if (title) { title.tabIndex = -1; title.focus({ preventScroll: true }); }
    }
    if (options.scroll && (changed || options.focus)) {
      const scrollRoot = document.scrollingElement || document.documentElement;
      if (typeof scrollRoot.scrollTo === 'function') scrollRoot.scrollTo({ top: 0, behavior: 'auto' });
      else scrollRoot.scrollTop = 0;
    }
    window.dispatchEvent(new CustomEvent('babel:workspacechange', { detail: { view: current } }));
    return true;
  }

  function revealField(field, options = {}) {
    if (!(field instanceof HTMLElement)) return;
    const panel = field.closest('[data-workspace-panel]');
    if (panel) navigate(panel.dataset.workspacePanel);
    const profile = field.closest('.provider-profile');
    const selector = byId(profile?.dataset.profileSelector || 'profile-selector');
    if (profile && selector) {
      const provider = profile.dataset.profileValue || profile.id.replace(/^profile-/, '');
      if ([...selector.options].some(option => option.value === provider)) {
        selector.value = provider;
        selector.dispatchEvent(new Event('change', { bubbles: true }));
      }
    }
    for (let ancestor = field.parentElement; ancestor; ancestor = ancestor.parentElement) {
      if (ancestor.tagName === 'DETAILS') ancestor.open = true;
      // Reveal a validation target within a conditional group without touching
      // any controls' disabled state or selecting another workspace panel.
      if (ancestor.hidden && !ancestor.matches('[data-workspace-panel]')) ancestor.hidden = false;
    }
    if (options.focus !== false) {
      field.focus({ preventScroll: true });
      field.scrollIntoView?.({ block: 'center', behavior: 'auto' });
    }
  }
  function labelFor(field) {
    const label = field.labels?.[0];
    const caption = label?.querySelector('span');
    return field.getAttribute('aria-label') || caption?.textContent?.trim() || label?.textContent?.trim() || field.name || field.id || t('workspace.this_field');
  }
  function noticeFor(field) {
    const dialog = field.closest('dialog');
    const id = dialog ? `workspace-validation-${dialog.id || 'dialog'}` : 'workspace-validation';
    let notice = byId(id);
    if (!notice) {
      notice = document.createElement('div'); notice.id = id; notice.className = 'notice error';
      notice.setAttribute('role', 'alert'); notice.hidden = true;
      if (dialog) (dialog.querySelector('.dialog-header') || dialog.firstElementChild)?.after(notice);
      else if (byId('error')) byId('error').after(notice);
      else (document.querySelector('main') || document.body).prepend(notice);
    }
    return notice;
  }
  function clearValidation() {
    if (!validation) return;
    const { field, notice } = validation;
    field.removeAttribute('aria-invalid');
    const descriptions = (field.getAttribute('aria-describedby') || '').split(/\s+/).filter(id => id && id !== notice.id);
    if (descriptions.length) field.setAttribute('aria-describedby', descriptions.join(' ')); else field.removeAttribute('aria-describedby');
    notice.hidden = true; validation = null;
  }
  function showInvalid(field) {
    clearValidation(); revealField(field);
    const notice = noticeFor(field);
    const message = t('workspace.invalid_field', { field: labelFor(field) });
    if (window.BabelI18n) window.BabelI18n.message(notice, message); else notice.textContent = message;
    notice.hidden = false;
    field.setAttribute('aria-invalid', 'true');
    const descriptions = new Set((field.getAttribute('aria-describedby') || '').split(/\s+/).filter(Boolean));
    descriptions.add(notice.id); field.setAttribute('aria-describedby', [...descriptions].join(' '));
    validation = { field, notice };
  }
  function reportValidity(container) {
    const fields = container?.elements ? [...container.elements] : [container];
    const invalid = fields.find(field => field?.willValidate && !field.validity.valid);
    if (!invalid) { clearValidation(); return true; }
    showInvalid(invalid);
    // Native validation of only this now-visible field gives its precise reason
    // without asking the browser to focus invalid fields in another hidden view.
    nativeReport = true;
    try { invalid.reportValidity(); } finally { nativeReport = false; }
    return false;
  }

  document.addEventListener('click', event => {
    const trigger = event.target.closest?.('[data-workspace-target], [data-workspace-field]');
    if (!trigger) return;
    const field = byId(trigger.dataset.workspaceField);
    if (field instanceof HTMLElement) {
      event.preventDefault(); revealField(field); return;
    }
    if (!views.includes(viewName(trigger.dataset.workspaceTarget))) return;
    event.preventDefault(); navigate(trigger.dataset.workspaceTarget, { focus: trigger.classList.contains('skip-link'), scroll: true });
  });
  document.addEventListener('keydown', event => {
    const trigger = event.target.closest?.('.nav-item[data-workspace-target]');
    if (!trigger || event.altKey || event.ctrlKey || event.metaKey) return;
    const buttons = navigation().filter(button => !button.disabled);
    const index = buttons.indexOf(trigger); if (index < 0) return;
    let next;
    if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = buttons.length - 1;
    else if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % buttons.length;
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + buttons.length) % buttons.length;
    else return;
    event.preventDefault(); buttons[next].focus(); navigate(buttons[next].dataset.workspaceTarget, { scroll: true });
  });
  document.addEventListener('invalid', event => {
    if (nativeReport) return;
    // Browser submit validation can emit one invalid event for every view.
    // Suppress that default UI and reveal only the first target for this cycle.
    event.preventDefault();
    if (!firstInvalid) {
      firstInvalid = event.target; showInvalid(firstInvalid);
      queueMicrotask(() => { firstInvalid = null; });
    }
  }, true);
  document.addEventListener('input', event => {
    if (validation?.field === event.target && event.target.validity.valid) clearValidation();
  });
  window.addEventListener('babel:languagechange', () => {
    renderHeader();
    if (validation) window.BabelI18n.message(validation.notice, t('workspace.invalid_field', { field: labelFor(validation.field) }));
  });
  window.BabelWorkspace = Object.freeze({ navigate, revealField, reportValidity, get current() { return current; } });

  // The fixed session dock can wrap after a language or viewport change.
  // Let the layout reserve its actual border-box height without polling.
  const dock = document.querySelector('.session-dock');
  if (dock) {
    let dockHeight = 0;
    const measureDock = () => {
      const height = Math.ceil(dock.getBoundingClientRect().height);
      if (Number.isFinite(height) && height > 0 && height !== dockHeight) {
        dockHeight = height;
        document.documentElement.style.setProperty('--session-dock-height', `${height}px`);
      }
    };
    measureDock();
    if (typeof ResizeObserver === 'function') new ResizeObserver(measureDock).observe(dock);
    else window.addEventListener('resize', measureDock);
    // With no measurable layout, the stylesheet's fallback remains in effect.
  }

  for (const [index, pane] of panes().entries()) if (!pane.id) pane.id = `workspace-${pane.dataset.workspacePanel}-${index + 1}`;
  for (const button of navigation()) {
    if (button.tagName === 'BUTTON') button.type = 'button';
    const ids = panes().filter(pane => viewName(pane.dataset.workspacePanel) === viewName(button.dataset.workspaceTarget)).map(pane => pane.id);
    if (ids.length) button.setAttribute('aria-controls', ids.join(' '));
  }
  let stored;
  try { stored = sessionStorage.getItem(storageKey); } catch (_) { /* Initial routing view is available without storage. */ }
  const initial = viewName(stored);
  navigate(views.includes(initial) ? initial : 'routing', { remember: stored !== initial });
})();
