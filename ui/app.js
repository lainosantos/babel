'use strict';

(() => {
  const byId = (id) => document.getElementById(id);
  const routeNames = ['microphone', 'speaker'];
  const form = byId('configuration');
  const state = { config: null, configRevision: null, configConflict: false, syncing: false, activity: 0, devices: [], status: null, autostart: null, platform: null, platformError: null, interface: null, busy: false, dirty: false, authenticated: false, polling: false, libraryBusy: false, libraryRoute: null, voices: { gemini: [], elevenlabs: [] } };
  const i18n = window.BabelI18n;
  const t = (key, values) => i18n.t(key, values);
  const filePathPreview = { revision: 0, timer: null, controller: null, phase: 'idle', paths: null, error: '' };
  let token = new URLSearchParams(location.hash.slice(1)).get('token');
  try {
    if (token) sessionStorage.setItem('babel-token', token);
    else token = sessionStorage.getItem('babel-token');
  } catch (_) { /* The in-memory capability still works when browser storage is disabled. */ }
  if (location.hash) history.replaceState(null, '', location.pathname + location.search);

  window.BabelDashboard = Object.freeze({ authorization: () => `Bearer ${token || ''}` });

  function announce(message) {
    i18n.message(byId('notice'), message);
    byId('notice').hidden = !message;
  }

  function showError(message) {
    i18n.message(byId('error'), message);
    byId('error').hidden = !message;
  }

  async function api(path, options = {}) {
    const response = await fetch(`/api${path}`, {
      method: options.method || 'GET',
      headers: { Authorization: `Bearer ${token || ''}`, ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...(options.revision != null ? { 'If-Match': `"${options.revision}"` } : {}) },
      body: options.body ? JSON.stringify(options.body) : undefined,
      cache: 'no-store',
      credentials: 'omit',
      signal: options.signal || AbortSignal.timeout(options.timeout || 35000),
    });
    const contentType = response.headers.get('content-type') || '';
    const body = contentType.includes('application/json') ? await response.json() : { error: await response.text() };
    if (!response.ok) {
      if (response.status === 401 || response.status === 403) {
        state.authenticated = false;
        updateControls();
      }
      if (response.status === 412) markConfigConflict();
      const error = new Error(body.error || t('error.http', { status: response.status }));
      error.status = response.status;
      throw error;
    }
    if (options.snapshot) {
      const revision = response.headers.get('etag')?.match(/^"(\d+)"$/)?.[1];
      if (revision === undefined) throw new Error(t("ui.could_not_verify_the_settings_revision_update_babel_and_reload_the_dashboar"));
      return { body, revision };
    }
    return body;
  }

  function markConfigConflict() {
    state.configConflict = true;
    byId('config-conflict').hidden = false;
    updateControls();
  }

  async function reloadConfig() {
    if (state.syncing) return;
    state.syncing = true;
    updateControls();
    try {
      const snapshot = await api('/config', { snapshot: true });
      state.configRevision = snapshot.revision;
      applyConfig(snapshot.body);
      await refreshInterface();
      state.configConflict = false;
      byId('config-conflict').hidden = true;
      refreshCredentialStatus(byId('profile-selector').value);
    } catch (error) { markConfigConflict(); throw error; }
    finally { state.syncing = false; updateControls(); }
  }

  async function synchronizeConfig(status) {
    if (state.configRevision === null || String(status.config_revision) === state.configRevision || state.configConflict) return;
    if (state.dirty) { markConfigConflict(); return; }
    await reloadConfig();
    announce(t("ui.settings_updated_from_the_tray_or_another_dashboard"));
  }

  function readValue(element) {
    if (element.type === 'checkbox') return element.checked;
    if (element.type === 'number' || element.type === 'range') return Number(element.value);
    return element.value;
  }

  function writeValue(element, value) {
    if (element.type === 'checkbox') element.checked = Boolean(value);
    else element.value = value == null ? '' : String(value);
  }

  function fieldContainer(config, element) {
    if (element.dataset.profile) return config.providers?.[element.dataset.profile];
    if (element.dataset.voiceRoute) return config[element.dataset.voiceRoute]?.voice;
    return config[element.dataset.route || element.dataset.section];
  }

  function routeProvider(route) { return byId(`${route}-provider`).value; }
  function profileValue(provider, field) { return byId(`profile-${provider}-${field}`)?.value || ''; }
  function isOpenAITranslation(model) { return model === 'gpt-realtime-translate' || model.startsWith('gpt-realtime-translate-'); }
  function dedicatedRoute(route) {
    const provider = routeProvider(route);
    const model = profileValue(provider, 'model').replace(/^models\//, '');
    return (provider === 'gemini' && model === 'gemini-3.5-live-translate-preview') || (provider === 'openai' && isOpenAITranslation(model));
  }

  function collectConfig() {
    const config = structuredClone(state.config);
    document.querySelectorAll('[data-field]').forEach((element) => {
      const container = fieldContainer(config, element);
      if (!container) return;
      const field = element.dataset.field;
      if (field === 'source_language' && element.dataset.savedSource !== undefined) container[field] = element.dataset.savedSource;
      else container[field] = readValue(element);
    });
    for (const route of routeNames) {
      if (config[route].enabled && dedicatedRoute(route)) {
        config[route].prompt = '';
        if (config[route].voice.engine === 'native') config[route].voice.voice_id = '';
      }
      if (config[route].voice.engine !== 'gemini') config[route].voice.style = '';
    }
    return config;
  }

  function applyConfig(config) {
    if (!config.providers) throw new Error(t("ui.these_settings_use_an_old_format_restart_the_updated_babel_to_load_provider"));
    state.config = config;
    writeValue(byId('files-base_path'), config.files?.base_path ?? '');
    document.querySelectorAll('[data-field]').forEach((element) => {
      const container = fieldContainer(config, element);
      delete element.dataset.savedSource;
      delete element.dataset.savedVoice;
      if (container && Object.hasOwn(container, element.dataset.field)) writeValue(element, container[element.dataset.field]);
      if (element.dataset.deviceDirection) delete element.dataset.initialized;
    });
    renderDevices();
    updateProviderControls();
    updateGainLabels();
    updateQualityHint();
    state.dirty = false;
    updateControls();
    scheduleFilePathPreview(0);
  }

  function validateBasePath() {
    const field = byId('files-base_path');
    const os = platformOs();
    const value = field.value;
    // Interpret only the host's path syntax; the browser may run on another OS.
    const windowsAbsolute = /^[a-z]:[\\/]/i.test(value)
        || /^\\\\\?\\[a-z]:\\/i.test(value)
        || /^\\\\\?\\UNC\\[^\\]+\\[^\\]+(?:\\|$)/i.test(value)
        || /^[\\/]{2}(?![?.](?:[\\/]|$))[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/.test(value);
    const unixAbsolute = value.startsWith('/');
    const absolute = os === 'windows' ? windowsAbsolute : os === 'unknown' ? windowsAbsolute || unixAbsolute : unixAbsolute;
    const message = !value || !absolute || value.includes('\0')
      ? t(os === 'windows' ? 'files.base_absolute_windows' : os === 'unknown' ? 'files.base_absolute_unknown' : 'files.base_absolute_unix') : '';
    field.placeholder = t(os === 'windows' ? 'files.base_placeholder_windows' : ['linux', 'macos'].includes(os) ? 'files.base_placeholder_unix' : 'files.base_placeholder');
    field.setCustomValidity(message);
    if (message) field.setAttribute('aria-invalid', 'true'); else field.removeAttribute('aria-invalid');
    const notice = byId('files-base-error');
    notice.textContent = message || (os === 'unknown' ? t('files.base_host_unavailable') : '');
    notice.hidden = !notice.textContent;
    notice.className = message ? 'file-path-error' : 'field-hint';
    notice.setAttribute('role', message ? 'alert' : 'status');
    return !message;
  }

  function renderFilePathPreview() {
    validateBasePath();
    const { phase, paths, error } = filePathPreview;
    const preview = byId('files-path-preview');
    preview.dataset.state = phase;
    preview.setAttribute('aria-busy', String(phase === 'loading'));
    for (const [id, key] of [['files-resolved-base', 'base_path'], ['transcription-resolved-directory', 'transcription_directory'], ['recording-resolved-directory', 'recording_directory']]) {
      byId(id).textContent = phase === 'ready' ? paths[key] : '—';
    }
    byId('files-path-status').textContent = t(`files.preview_${phase}`);
    byId('files-path-error').hidden = phase !== 'error';
    byId('files-path-error').textContent = phase === 'error' ? t('files.preview_error_detail', { error }) : '';
  }

  function scheduleFilePathPreview(delay = 300) {
    if (!state.authenticated || !state.config) return;
    clearTimeout(filePathPreview.timer);
    filePathPreview.controller?.abort();
    const revision = ++filePathPreview.revision;
    filePathPreview.phase = !validateBasePath() ? 'invalid' : platformOs() === 'unknown' ? 'unavailable' : 'loading'; filePathPreview.paths = null; filePathPreview.error = '';
    renderFilePathPreview();
    if (filePathPreview.phase === 'loading') filePathPreview.timer = setTimeout(() => refreshFilePathPreview(revision), delay);
  }

  async function refreshFilePathPreview(revision) {
    if (revision !== filePathPreview.revision) return;
    const controller = new AbortController();
    filePathPreview.controller = controller;
    let timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; controller.abort(); }, 10000);
    const body = {
      base_path: byId('files-base_path').value,
      transcription_directory: byId('transcription-directory').value,
      recording_directory: byId('recording-directory').value,
    };
    try {
      const paths = await api('/file-paths', { method: 'POST', body, signal: controller.signal });
      if (revision !== filePathPreview.revision) return;
      if (!paths || ['base_path', 'transcription_directory', 'recording_directory'].some(key => typeof paths[key] !== 'string' || !paths[key] || paths[key].includes('\0'))) throw new Error(t('files.preview_invalid_response'));
      filePathPreview.paths = paths; filePathPreview.phase = 'ready';
    } catch (error) {
      if (revision !== filePathPreview.revision) return;
      filePathPreview.phase = error.status === 404 ? 'unsupported' : 'error';
      filePathPreview.error = timedOut ? t('files.preview_timeout') : error.message;
    } finally {
      clearTimeout(timeout);
      if (revision === filePathPreview.revision) { filePathPreview.controller = null; renderFilePathPreview(); }
    }
  }

  function renderDevices() {
    for (const select of document.querySelectorAll('[data-device-direction]')) {
      const selected = select.dataset.initialized ? select.value : state.config?.[select.dataset.route]?.[select.dataset.field] || '';
      const options = state.devices.filter((device) => device.direction === select.dataset.deviceDirection);
      select.replaceChildren(new Option(t("ui.select_a_device"), ''));
      for (const device of options) select.add(new Option(device.is_virtual ? t('device.virtual', { name: device.name }) : device.name, device.id));
      if (selected && !options.some((device) => device.id === selected)) select.add(new Option(t('device.unavailable', { name: selected }), selected));
      select.value = selected;
      select.dataset.initialized = 'true';
      for (const option of select.options) {
        const device = options.find(item => item.id === option.value);
        option.dataset.deviceName = device?.name || option.value;
        option.dataset.deviceVirtual = String(Boolean(device?.is_virtual));
        option.dataset.deviceUnavailable = String(Boolean(option.value && !device));
      }
    }
  }

  async function refreshDevices() { state.devices = await api('/devices'); renderDevices(); renderPlatformEndpoints(); }

  function platformOs() { return ['linux', 'macos', 'windows'].includes(state.platform?.os) ? state.platform.os : 'unknown'; }
  function managesVirtualDevices() { return platformOs() === 'linux' && state.platform?.manages_virtual_devices === true; }
  function supportsHostAutostart() {
    const methods = { linux: 'xdg', macos: 'launch_agent', windows: 'registry_run' };
    return Object.hasOwn(methods, platformOs()) && state.platform?.autostart_method === methods[platformOs()];
  }
  function platformText(id, key, values) {
    const node = byId(id);
    // Static i18n loading text must not overwrite host metadata on language changes.
    delete node.dataset.i18n;
    node.textContent = t(key, values);
  }
  function renderPlatformEndpoints() {
    const os = platformOs();
    for (const route of routeNames) {
      const control = byId(`${route}-${route === 'microphone' ? 'playback_device' : 'capture_device'}`);
      const device = state.devices.find(item => item.id === control.value && item.is_virtual);
      const fallback = route === 'microphone' ? 'platform.virtual_microphone' : 'platform.virtual_output';
      const node = byId(`${route}-virtual-label`); delete node.dataset.i18n;
      if (os === 'linux') node.textContent = route === 'microphone' ? 'Babel_Microphone' : 'Babel_Speaker';
      else if (os === 'macos' && device) node.textContent = device.name;
      else node.textContent = t(fallback);
    }
  }
  function renderNativeDeviceGuide(os) {
    let guide = byId('platform-native-guide');
    if (!['macos', 'windows'].includes(os)) { if (guide) guide.hidden = true; return; }
    if (!guide) {
      guide = document.createElement('details'); guide.id = 'platform-native-guide'; guide.className = 'native-device-guide';
      byId('platform-device-permissions').after(guide);
    }
    guide.hidden = false;
    if (guide.dataset.os !== os) {
      guide.dataset.os = os; guide.replaceChildren();
      const text = (tag, key) => {
        const node = document.createElement(tag); node.dataset.i18n = key; node.textContent = t(key); return node;
      };
      guide.append(text('summary', 'platform.native_mapping'));
      const scroll = document.createElement('div'); scroll.className = 'native-device-table';
      const table = document.createElement('table');
      table.append(text('caption', 'platform.native_mapping_hint'));
      const header = document.createElement('thead'); const heading = document.createElement('tr');
      for (const key of ['platform.mapping_where', 'platform.mapping_device']) { const cell = text('th', key); cell.scope = 'col'; heading.append(cell); }
      header.append(heading); table.append(header);
      const body = document.createElement('tbody');
      const endpoints = os === 'macos' ? ['Babel Microphone', 'Babel Microphone', 'Babel Speaker', 'Babel Speaker'] : ['Babel Microphone Feed', 'Babel Microphone', 'Babel Speaker', 'Babel Speaker Monitor'];
      for (const [index, key] of ['mic_capture', 'mic_playback', 'app_microphone', 'app_speaker', 'speaker_capture', 'speaker_playback'].entries()) {
        const row = document.createElement('tr'); const label = text('th', `platform.mapping_${key}`); label.scope = 'row';
        const value = index === 0 ? text('td', 'platform.mapping_physical_microphone') : index === 5 ? text('td', 'platform.mapping_physical_output') : document.createElement('td');
        if (index > 0 && index < 5) value.textContent = endpoints[index - 1];
        row.append(label, value); body.append(row);
      }
      table.append(body); scroll.append(table); guide.append(scroll);
      guide.append(text('p', `platform.${os}_driver_status`), text('p', 'platform.native_install_manual'), text('p', `platform.${os}_activity`), text('p', `platform.${os}_external`));
      const links = document.createElement('div'); links.className = 'platform-help';
      const urls = ['/help/native-drivers', '/help/platforms'];
      for (const [index, key] of ['platform.native_driver_guide', 'platform.native_routing_guide'].entries()) {
        const link = text('a', key); link.href = urls[index]; link.target = '_blank'; link.rel = 'noopener noreferrer'; links.append(link);
      }
      guide.append(links);
    }
    i18n.apply(guide);
  }
  function renderPlatform() {
    const os = platformOs();
    const names = { linux: 'Linux', macos: 'macOS', windows: 'Windows' };
    const backends = { pulseaudio: 'PulseAudio / PipeWire-pulse', coreaudio: 'CoreAudio', wasapi: 'WASAPI' };
    if (!state.platform) platformText('platform-summary', state.platformError ? 'platform.unavailable' : 'platform.detecting');
    else if (os === 'unknown') platformText('platform-summary', 'platform.unknown');
    else platformText('platform-summary', 'platform.summary', { name: names[os], backend: backends[state.platform.audio_backend] || t('platform.unsupported_backend') });
    platformText('platform-device-setup', `platform.${os}_setup`);
    platformText('platform-device-details', `platform.${os}_details`);
    platformText('platform-device-permissions', `platform.${os}_permissions`);
    platformText('platform-autostart', supportsHostAutostart() ? `platform.${os}_autostart` : 'platform.autostart_unavailable');
    platformText('platform-guide', os === 'unknown' ? 'platform.current_guide' : 'platform.named_guide', { name: names[os] || '' });
    byId('platform-error').hidden = !state.platformError;
    if (state.platformError) platformText('platform-error', 'platform.read_error', { error: state.platformError });
    byId('install-devices').hidden = !managesVirtualDevices();
    byId('uninstall-devices').hidden = !managesVirtualDevices();
    byId('device-external-guide').hidden = !['macos', 'windows'].includes(os);
    if (os === 'macos' || os === 'windows') {
      platformText('device-external-guide', `platform.${os}_install`);
      byId('device-external-guide').href = '/help/native-drivers';
    }
    renderNativeDeviceGuide(os);
    renderPlatformEndpoints(); updateControls();
  }
  async function refreshPlatform() {
    try {
      const metadata = await api('/platform');
      if (!metadata || typeof metadata !== 'object' || !['linux', 'macos', 'windows', 'unknown'].includes(metadata.os)) throw new Error(t('platform.invalid_metadata'));
      state.platform = metadata; state.platformError = null;
    } catch (error) { state.platform = null; state.platformError = error.message; }
    renderPlatform();
    scheduleFilePathPreview(0);
  }

  function renderVoiceOptions() {
    for (const route of routeNames) {
      const engine = byId(`${route}-voice-engine`).value;
      const provider = engine === 'native' ? routeProvider(route) : engine;
      const options = byId(`${route}-voice-options`);
      options.replaceChildren();
      if (engine !== 'native') for (const voice of state.voices[provider] || []) options.append(new Option(`${voice.name} · ${voiceKind(voice.kind)}`, voice.id));
      if (engine === 'native') {
        const defaults = provider === 'gemini' ? ['Kore', 'Puck', 'Charon', 'Fenrir', 'Aoede'] : provider === 'openai' ? ['marin', 'cedar', 'alloy', 'ash', 'coral', 'sage', 'verse'] : [];
        for (const name of defaults) options.append(new Option(name, name));
      }
    }
  }

  function updateProviderControls() {
    for (const route of routeNames) {
      const translating = byId(`${route}-enabled`).checked;
      const dedicated = dedicatedRoute(route) && translating;
      const provider = routeProvider(route);
      const source = byId(`${route}-source_language`);
      source.disabled = dedicated;
      if (dedicated) { if (source.dataset.savedSource === undefined) source.dataset.savedSource = source.value; source.value = t("ui.automatic"); }
      else if (!dedicated && source.dataset.savedSource !== undefined) { source.value = source.dataset.savedSource; delete source.dataset.savedSource; }
      byId(`${route}-target_language`).disabled = !translating;
      byId(`${route}-gain`).disabled = !translating;
      byId(`${route}-prompt`).disabled = !translating || dedicated;
      byId(`${route}-prompt-hint`).textContent = dedicated
        ? t("ui.this_continuous_model_does_not_accept_prompts_saving_in_this_mode_clears_th")
        : !translating ? t("ui.translation_instructions_are_not_used_while_this_translation_is_off")
        : t("ui.add_tone_terminology_and_proper_names_to_guide_translation");
      byId(`${route}-provider-hint`).textContent = !translating
        ? t("ui.original_audio_if_transcription_is_enabled_for_this_source_this_provider_re")
        : provider === 'local' ? t("ui.transcription_translation_and_voice_through_local_services_in_segments")
        : dedicated ? t("ui.continuous_translation_with_automatic_source_language_detection") : t("ui.conversation_model_usually_waits_for_pauses_before_responding");
      const engineControl = byId(`${route}-voice-engine`);
      engineControl.disabled = !translating;
      const native = engineControl.value === 'native';
      const voice = byId(`${route}-voice-voice_id`);
      const automaticVoice = native && dedicated;
      voice.disabled = automaticVoice || !translating;
      if (automaticVoice && voice.dataset.savedVoice === undefined) { voice.dataset.savedVoice = voice.value; voice.value = ''; }
      else if (!automaticVoice && voice.dataset.savedVoice !== undefined) { voice.value = voice.dataset.savedVoice; delete voice.dataset.savedVoice; }
      voice.placeholder = automaticVoice ? (provider === 'gemini' ? t("ui.automatic_preservation_gemini") : t("ui.model_s_native_voice")) : provider === 'local' && native ? t("ui.piper_voice_or_profile_default") : t("ui.profile_s_default_voice_or_id");
      byId(`${route}-voice-style`).disabled = engineControl.value !== 'gemini' || !translating;
      byId(`${route}-voice-chunk_ms`).disabled = native || !translating;
      byId(`${route}-voice-hint`).textContent = !translating ? t("ui.this_route_transmits_the_original_voice_synthesis_options_are_only_used_wit")
        : native ? (dedicated ? (provider === 'gemini' ? t("ui.gemini_live_translate_attempts_to_preserve_original_voice_characteristics_w_2") : t("ui.openai_translate_uses_the_model_s_native_voice_vocal_identity_preservation_")) : provider === 'local' ? t("ui.uses_the_piper_service_enter_a_voice_available_in_that_service_or_leave_emp") : t("ui.uses_the_translator_s_own_audio_output_and_the_profile_s_default_voice_when"))
        : t("ui.re_synthesizes_translated_text_with_this_fixed_voice_adds_latency_translate");
    }
    byId('profile-gemini-hint').textContent = t("ui.gemini_live_translate_continuous_translation_without_prompts_or_a_fixed_nat");
    byId('profile-openai-hint').textContent = t("ui.gpt_realtime_translate_continuous_translation_gpt_realtime_2_1_conversation");
    byId('profile-elevenlabs-hint').textContent = t("ui.additional_voice_synthesis_using_elevenlabs_library_ids_configure_the_route");
    byId('profile-gemini-voice').disabled = profileValue('gemini', 'model').replace(/^models\//, '') === 'gemini-3.5-live-translate-preview';
    byId('profile-openai-voice').disabled = isOpenAITranslation(profileValue('openai', 'model'));
    const local = routeNames.every(route => {
      const translates = byId(`${route}-enabled`).checked;
      const transcribes = byId('transcription-enabled').checked && byId(`transcription-${route}`).checked;
      return (!translates && !transcribes) || (routeProvider(route) === 'local' && (!translates || byId(`${route}-voice-engine`).value === 'native'));
    });
    byId('footer-state').textContent = local ? t("ui.session_configured_for_local_processing") : t("ui.session_configured_with_cloud_providers");
    renderVoiceOptions();
    renderSignalPaths();
  }

  function routeWaiting(route, status = state.status) {
    return status?.[route]?.state === 'waiting_for_app' && !status[route].device_error;
  }

  function routeActive(route, status = state.status) {
    const current = status?.[route];
    const inactive = ['waiting_for_app', 'unconfigured', 'stopped', 'error', 'failed', 'disabled'];
    return Boolean((status?.running || status?.routing_active) && current && !current.device_error && !inactive.includes(current.state));
  }

  function waitingHint(route) {
    return t(route === 'microphone' ? 'routing.waiting_microphone' : 'routing.waiting_speaker');
  }

  function renderSignalPaths() {
    renderPlatformEndpoints();
    for (const route of routeNames) {
      const active = routeActive(route);
      const translating = active && state.status?.running && state.config?.[route]?.enabled;
      const node = byId(`${route}-signal`);
      node.textContent = !active ? '—' : translating ? t("ui.ai") : t("ui.original");
      node.classList.toggle('original', Boolean(active && !translating));
      node.parentElement.setAttribute('aria-label', `${route === 'microphone' ? t("ui.physical_microphone_to_virtual_microphone") : t("ui.virtual_output_to_headphones")}: ${routeWaiting(route) ? waitingHint(route) : !active ? t("ui.routing_unavailable") : translating ? t("ui.ai_translation") : t("ui.original_audio_routing")}.`);
      byId(`${route}-output-label`).textContent = translating ? t("ui.translated") : t("ui.original");
    }
  }

  function updateQualityHint() {
    const descriptions = {
      low_latency: t("ui.conversation_10_ms_packets_and_200_ms_silence"),
      balanced: t("ui.conversation_20_ms_packets_and_400_ms_silence"),
      high_quality: t("ui.conversation_40_ms_packets_and_700_ms_silence"),
    };
    byId('quality-hint').textContent = t('audio.quality_hint', { description: descriptions[byId('audio-quality').value] || '' });
  }

  function updateGainLabels() {
    for (const route of routeNames) byId(`${route}-gain-value`).textContent = i18n.number(Number(byId(`${route}-gain`).value), { style: 'percent', maximumFractionDigits: 0 });
  }

  function updateControls() {
    const running = Boolean(state.status?.running);
    const processingSelected = routeNames.some(route => byId(`${route}-enabled`).checked) || byId('transcription-enabled').checked || byId('recording-enabled').checked;
    const unavailable = state.busy || state.syncing || !state.authenticated || !state.config || !state.status;
    byId('interface-language').disabled = state.busy || state.syncing || !state.authenticated;
    byId('settings').disabled = running || unavailable;
    byId('session-name').disabled = running || unavailable;
    byId('save').disabled = running || unavailable || !state.dirty || state.configConflict;
    byId('start').disabled = running || unavailable || state.configConflict || !processingSelected;
    byId('reload-config').disabled = state.busy || state.syncing || !state.authenticated;
    byId('start').hidden = running;
    byId('stop').hidden = !running;
    byId('stop').disabled = state.busy || !state.authenticated;
    byId('install-devices').disabled = running || unavailable || !managesVirtualDevices();
    byId('uninstall-devices').disabled = running || unavailable || !managesVirtualDevices();
    byId('refresh-devices').disabled = state.busy || !state.authenticated;
    const startupUnavailable = state.busy || !state.authenticated || !state.autostart?.supported || !supportsHostAutostart();
    byId('autostart-enabled').disabled = startupUnavailable;
    byId('autostart-apply').disabled = startupUnavailable || byId('autostart-enabled').checked === state.autostart?.enabled;
    if (byId('voice-library').open) {
      libraryControls();
      document.querySelectorAll('#library-voices button').forEach(button => { button.disabled = running || state.busy || state.syncing || state.libraryBusy || button.dataset.verificationRequired === 'true'; });
    }
    const transcriptionEnabled = byId('transcription-enabled').checked;
    for (const field of ['microphone', 'speaker', 'timestamps']) byId(`transcription-${field}`).disabled = !transcriptionEnabled;
    for (const field of ['microphone', 'speaker']) byId(`recording-${field}`).disabled = !byId('recording-enabled').checked;
    byId('save-state').textContent = state.configConflict ? t("ui.reload_settings") : state.dirty ? t("ui.unsaved_settings") : t("ui.settings_saved");
    byId('save-state').classList.toggle('dirty', state.dirty || state.configConflict);
    byId('session-hint').textContent = !state.authenticated
      ? t("ui.reopen_the_full_address_shown_in_the_terminal_to_connect_this_dashboard")
      : state.configConflict
        ? t("ui.settings_changed_outside_this_dashboard_reload_saved_settings_before_saving")
      : running
        ? t("ui.session_active_end_it_to_change_settings_physical_devices_can_be_switched_f")
      : !processingSelected
        ? t("ui.original_audio_routing_works_without_a_session_enable_translation_transcrip")
        : state.dirty
          ? t("ui.you_have_unsaved_settings_starting_the_session_also_saves_them")
          : t("ui.choose_which_features_to_use_and_start_a_session_to_translate_transcribe_or");
  }

  function renderStatus(status) {
    state.status = status;
    const sessionIdentity = status.session_name || status.session_id;
    const sessionLabel = sessionIdentity ? t(status.running ? 'session.identity' : 'session.previous', { name: sessionIdentity }) : '';
    if (byId('session-identity').textContent !== sessionLabel) byId('session-identity').textContent = sessionLabel;
    byId('session-identity').hidden = !sessionIdentity;
    const waiting = routeNames.every(route => routeWaiting(route, status));
    byId('session-state').textContent = status.running ? t("ui.session_active") : waiting ? t('routing.waiting_for_app') : status.routing_active ? t("ui.original_audio") : t("ui.no_routing");
    byId('status-dot').className = `status-dot${status.running || status.routing_active ? ' running' : ''}${status.last_error || status.routing_error ? ' error' : ''}`;
    byId('session-error').textContent = status.last_error || '';
    byId('session-error').hidden = !status.last_error;
    byId('routing-error').textContent = status.routing_error ? t('routing.error', { error: status.routing_error }) : '';
    byId('routing-error').hidden = !status.routing_error;
    const translations = { stopped: t("ui.stopped"), unconfigured: t('routing.unconfigured'), passthrough: t("ui.original_audio"), transcribing: t("ui.transcribing_original_audio"), starting: t("ui.starting"), connecting: t("ui.connecting_to_ai"), running: t("ui.running"), streaming: t("ui.streaming"), translating: t("ui.translating"), reconnecting: t("ui.reconnecting"), error: t("ui.error"), failed: t("ui.failed"), disabled: t("ui.disabled") };
    for (const route of routeNames) {
      const current = status[route];
      if (!current) continue;
      byId(`${route}-state`).textContent = current.device_error ? t("ui.device_unavailable") : routeWaiting(route, status) ? t('routing.waiting_for_app') : translations[current.state] || current.state || t("ui.stopped");
      byId(`${route}-state`).title = routeWaiting(route, status) ? waitingHint(route) : '';
      byId(`${route}-reconnects`).textContent = t('audio.reconnections', { count: i18n.number(current.reconnects || 0) });
      for (const metric of ['captured_frames', 'dropped_frames', 'underruns']) {
        byId(`${route}-${metric}`).textContent = i18n.number(current[metric] || 0);
        byId(`${route}-${metric}`).classList.toggle('nonzero', metric !== 'captured_frames' && current[metric] > 0);
      }
      for (const side of ['input', 'output']) {
        const raw = routeActive(route, status) ? Number(current[`${side}_level`]) || 0 : 0;
        const level = Math.max(0, Math.min(1, raw));
        byId(`${route}-${side}`).value = level;
        byId(`${route}-${side}-db`).textContent = raw > 0.0001 ? `${i18n.number(20 * Math.log10(raw), { maximumFractionDigits: 0 })} dB` : '−∞ dB';
      }
      const transcript = current.last_input_transcript;
      byId(`${route}-transcripts`).hidden = !transcript;
      byId(`${route}-input-transcript`).textContent = transcript || t("ui.waiting_for_audio");
    }
    renderSignalPaths();
    updateControls();
  }

  async function pollStatus() {
    if (!state.authenticated || state.polling || state.busy || state.syncing || document.hidden) return;
    state.polling = true;
    const activity = state.activity;
    try {
      const status = await api('/status');
      if (state.busy || state.activity !== activity) return;
      renderStatus(status);
      await synchronizeConfig(status);
    } catch (error) {
      byId('session-state').textContent = t("ui.disconnected");
      byId('status-dot').className = 'status-dot error';
      showError(t('error.connection', { error: error.message }));
    } finally { state.polling = false; }
  }

  async function action(operation) {
    if (state.busy || state.syncing) return;
    state.busy = true;
    state.activity++;
    updateControls();
    showError('');
    announce('');
    try { await operation(); }
    catch (error) { showError(error.name === 'TimeoutError' ? t("ui.babel_took_too_long_to_respond_check_the_terminal_and_try_again") : error.message); }
    finally { state.busy = false; updateControls(); }
  }

  async function saveConfig() {
    if (state.configConflict || state.configRevision === null) throw new Error(t("ui.reload_saved_settings_before_continuing"));
    const config = collectConfig();
    const saved = await api('/config', { method: 'PUT', body: config, revision: state.configRevision, snapshot: true });
    state.configRevision = saved.revision;
    state.config = config;
    state.dirty = false;
    updateControls();
  }

  form.addEventListener('submit', (event) => event.preventDefault());
  form.addEventListener('input', (event) => {
    if (!event.target.matches('[data-field]')) return;
    state.dirty = true;
    updateGainLabels();
    if (['provider', 'enabled', 'engine', 'model'].includes(event.target.dataset.field) || event.target.dataset.section === 'transcription') updateProviderControls();
    if (event.target.id === 'audio-quality') updateQualityHint();
    if (['files-base_path', 'transcription-directory', 'recording-directory'].includes(event.target.id)) scheduleFilePathPreview();
    updateControls();
  });
  // Some select controls emit change without input in older embedded browsers.
  form.addEventListener('change', (event) => {
    if (event.target.matches('select')) event.target.dispatchEvent(new Event('input', { bubbles: true }));
  });
  function reportWorkspaceValidity(container) {
    return window.BabelWorkspace ? window.BabelWorkspace.reportValidity(container) : container.reportValidity();
  }
  byId('save').addEventListener('click', () => {
    if (!reportWorkspaceValidity(form)) return;
    action(async () => { await saveConfig(); announce(t("ui.settings_saved_2")); });
  });
  byId('start').addEventListener('click', () => {
    if (state.configConflict || state.configRevision === null) return;
    if (!reportWorkspaceValidity(form)) return;
    if (!reportWorkspaceValidity(byId('session-name'))) return;
    const name = byId('session-name').value.trim();
    action(async () => {
      if (state.dirty) await saveConfig();
      await api('/start', { method: 'POST', revision: state.configRevision, ...(name ? { body: { name } } : {}) });
      renderStatus(await api('/status'));
      announce(t("ui.session_started_with_the_selected_features_original_audio_continues_on_rout"));
    });
  });
  byId('stop').addEventListener('click', () => action(async () => {
    await api('/stop', { method: 'POST' });
    renderStatus(await api('/status'));
    announce(t("ui.session_ended_original_audio_passes_through_the_configured_devices_again"));
  }));
  byId('reload-config').addEventListener('click', () => action(async () => { await reloadConfig(); announce(t("ui.saved_settings_reloaded_review_devices_before_starting")); }));
  byId('refresh-devices').addEventListener('click', () => action(async () => { await Promise.all([refreshDevices(), refreshPlatform()]); announce(t("ui.device_list_refreshed")); }));
  function renderAutostart(status) {
    state.autostart = status;
    byId('autostart-enabled').checked = Boolean(status.enabled);
    renderAutostartDescription();
    updateControls();
  }
  function renderAutostartDescription() {
    const status = state.autostart;
    if (!status) return;
    const parts = [!status.supported ? t('autostart.unsupported') : status.enabled ? t('ui.autostart_enabled_babel_opens_with_local_original_audio_routing_without_sta') : t('ui.autostart_disabled')];
    const methods = { systemd_user: 'autostart.method_systemd_user', xdg: 'autostart.method_xdg', launch_agent: 'autostart.method_launch_agent', registry_run: 'autostart.method_registry_run' };
    if (status.supported && Object.hasOwn(methods, status.method)) {
      parts.push(t(methods[status.method]));
      if (typeof status.entry_path === 'string' && status.entry_path) parts.push(t('autostart.entry_path', { path: status.entry_path }));
    }
    byId('autostart-status').textContent = parts.join(' ');
  }
  async function refreshAutostart() { renderAutostart(await api('/autostart')); }
  byId('autostart-enabled').addEventListener('change', updateControls);
  byId('autostart-apply').addEventListener('click', () => {
    const enabled = byId('autostart-enabled').checked;
    action(async () => {
      renderAutostart(await api('/autostart', { method: 'POST', body: { enabled } }));
      announce(enabled ? t("ui.autostart_enabled_babel_will_open_in_the_tray_with_original_audio_without_s") : t("ui.autostart_disabled"));
    });
  });
  for (const operation of ['install', 'uninstall']) {
    byId(`${operation}-devices`).addEventListener('click', () => action(async () => {
      if (!managesVirtualDevices()) throw new Error(t('platform.management_unavailable'));
      const result = await api(`/virtual/${operation}`, { method: 'POST' });
      announce(result.message || t("ui.devices_updated"));
      await refreshDevices();
    }));
  }
  addEventListener('beforeunload', (event) => { if (state.dirty) event.preventDefault(); });
  document.addEventListener('visibilitychange', () => { if (!document.hidden) pollStatus(); });

  function profileChanged() {
    const provider = byId('profile-selector').value;
    for (const name of ['gemini', 'openai', 'elevenlabs', 'local']) byId(`profile-${name}`).hidden = name !== provider;
    refreshCredentialStatus(provider);
  }

  async function refreshCredentialStatus(provider) {
    if (provider === 'local' || !state.authenticated) return;
    const environment = profileValue(provider, 'api_key_env');
    if (!environment) return;
    try {
      const result = await api(`/credentials?${new URLSearchParams({ api_key_env: environment })}`);
      if (profileValue(provider, 'api_key_env') !== environment) return;
      i18n.message(byId(`credential-${provider}-status`), result.configured
        ? t("ui.a_key_is_available_for_this_profile_in_memory_or_the_environment_its_value_")
        : t("ui.no_key_is_available_apply_a_temporary_key_or_set_the_environment_variable_b"));
    } catch (error) { byId(`credential-${provider}-status`).textContent = error.message; }
  }

  byId('profile-selector').addEventListener('change', profileChanged);
  for (const provider of ['gemini', 'openai', 'elevenlabs']) {
    byId(`profile-${provider}-api_key_env`).addEventListener('change', () => refreshCredentialStatus(provider));
  }
  document.querySelectorAll('.credential-apply').forEach(button => button.addEventListener('click', () => action(async () => {
    const provider = button.dataset.provider;
    const input = byId(`credential-${provider}`);
    const key = input.value;
    input.value = '';
    if (!key.trim()) throw new Error(t("ui.enter_a_key_for_this_app_instance"));
    await api('/credentials', { method: 'POST', body: { api_key_env: profileValue(provider, 'api_key_env'), key } });
    await refreshCredentialStatus(provider);
    announce(t("ui.key_applied_to_this_app_instance_s_memory_only_it_will_be_discarded_when_ba"));
  })));
  document.querySelectorAll('.credential-clear').forEach(button => button.addEventListener('click', () => action(async () => {
    const provider = button.dataset.provider;
    byId(`credential-${provider}`).value = '';
    await api('/credentials/clear', { method: 'POST', body: { api_key_env: profileValue(provider, 'api_key_env') } });
    await refreshCredentialStatus(provider);
    announce(t("ui.temporary_key_removed_a_key_set_in_the_environment_remains_available_as_a_f"));
  })));

  const dialog = byId('voice-library');
  function voiceKind(kind) {
    const names = { preset: t("ui.preset"), prebuilt: t("ui.preset"), designed: t("ui.designed"), generated: t("ui.designed"), cloned: t("ui.cloned"), professional: t("ui.professional_clone"), instant: t("ui.instant_clone"), verification_required: t("ui.verification_required"), created: t("ui.created") };
    return names[String(kind).toLowerCase()] || kind;
  }
  function libraryMessage(kind, message) {
    const element = byId(`library-${kind}`);
    i18n.message(element, message);
    element.hidden = !message;
  }
  function libraryControls() {
    byId('library-refresh').disabled = state.libraryBusy || !state.authenticated;
    byId('library-provider').disabled = state.libraryBusy;
    byId('voice-create-fields').disabled = state.libraryBusy || Boolean(state.status?.running) || !state.authenticated;
    const provider = byId('library-provider').value;
    const clone = byId('voice-create-method').value === 'clone';
    const name = provider === 'gemini' ? 'Google Gemini' : 'ElevenLabs';
    byId('voice-design-fields').hidden = clone;
    byId('voice-clone-fields').hidden = !clone;
    byId('voice-consent-label').hidden = provider !== 'gemini';
    byId('voice-create-description').required = !clone;
    byId('voice-create-reference').required = clone;
    byId('voice-create-consent').required = clone && provider === 'gemini';
    byId('voice-create-submit').textContent = state.libraryBusy ? t("ui.waiting_for_the_provider") : t(clone ? 'voice.clone' : 'voice.create', { provider: name });
    byId('library-hint').textContent = t('voice.library_hint', { provider: name });
    byId('voice-create-disclosure').textContent = clone
      ? t(provider === 'gemini' ? 'voice.clone_consent_disclosure' : 'voice.clone_disclosure', { provider: name })
      : t('voice.design_disclosure', { provider: name });
  }

  function useVoice(voice, route) {
    if (state.status?.running || state.busy || state.syncing) return;
    const input = byId(`${route}-voice-voice_id`);
    delete input.dataset.savedVoice;
    byId(`${route}-voice-engine`).value = voice.provider;
    input.value = voice.id;
    state.dirty = true;
    updateProviderControls();
    updateControls();
    dialog.close();
    announce(t(`voice.selected_${route}`, { name: voice.name }));
  }

  function renderLibrary() {
    const provider = byId('library-provider').value;
    const container = byId('library-voices');
    const voices = state.voices[provider] || [];
    container.replaceChildren();
    if (!voices.length) {
      const empty = document.createElement('p');
      empty.className = 'library-empty';
      empty.dataset.i18n = 'ui.no_voices_loaded_use_load_voices_or_create_a_voice_in_this_account';
      empty.textContent = t(empty.dataset.i18n);
      container.append(empty);
    }
    for (const voice of voices) {
      const row = document.createElement('div'); row.className = 'voice-item';
      const description = document.createElement('div');
      const title = document.createElement('strong'); title.textContent = voice.name;
      const detail = document.createElement('span'); detail.dataset.voiceKind = voice.kind; detail.dataset.voiceId = voice.id; detail.textContent = `${voiceKind(voice.kind)} · ${voice.id}`;
      description.append(title, detail);
      const actions = document.createElement('div'); actions.className = 'voice-item-actions';
      for (const route of routeNames) {
        const button = document.createElement('button'); button.type = 'button'; button.className = 'button quiet';
        button.dataset.i18n = route === 'microphone' ? 'ui.use_for_microphone' : 'ui.use_for_output';
        button.textContent = t(button.dataset.i18n);
        button.disabled = Boolean(state.status?.running) || state.libraryBusy || voice.kind === 'verification_required';
        button.dataset.verificationRequired = String(voice.kind === 'verification_required');
        if (voice.kind === 'verification_required') { button.dataset.i18nTitle = "ui.complete_verification_of_this_voice_in_the_provider_s_account_before_using_"; button.title = t(button.dataset.i18nTitle); }
        button.addEventListener('click', () => useVoice(voice, route));
        actions.append(button);
      }
      row.append(description, actions); container.append(row);
    }
    renderVoiceOptions();
  }

  document.querySelectorAll('.library-open').forEach(button => button.addEventListener('click', () => {
    const route = button.dataset.libraryRoute;
    state.libraryRoute = route || null;
    const engine = route ? byId(`${route}-voice-engine`).value : byId('profile-selector').value;
    byId('library-provider').value = engine === 'elevenlabs' ? 'elevenlabs' : 'gemini';
    libraryMessage('error', ''); libraryMessage('notice', '');
    libraryControls(); renderLibrary(); dialog.showModal();
  }));
  byId('library-close').addEventListener('click', () => dialog.close());
  byId('library-provider').addEventListener('change', () => { libraryControls(); renderLibrary(); libraryMessage('notice', ''); libraryMessage('error', ''); });
  byId('voice-create-method').addEventListener('change', libraryControls);

  async function libraryAction(operation) {
    if (state.libraryBusy) return;
    state.libraryBusy = true; libraryControls(); renderLibrary();
    libraryMessage('error', ''); libraryMessage('notice', '');
    try { await operation(); }
    catch (error) { libraryMessage('error', error.name === 'TimeoutError' ? t("ui.the_provider_took_too_long_to_respond_reload_the_library_to_check_whether_t") : error.message); }
    finally { state.libraryBusy = false; libraryControls(); renderLibrary(); }
  }

  byId('library-refresh').addEventListener('click', () => libraryAction(async () => {
    const provider = byId('library-provider').value;
    state.voices[provider] = await api(`/voices?${new URLSearchParams({ provider, api_key_env: profileValue(provider, 'api_key_env') })}`, { timeout: 75000 });
    const count = state.voices[provider].length;
    libraryMessage('notice', t(`voice.loaded_${i18n.plural(count)}`, { count: i18n.number(count) }));
  }));

  function wavBase64(file, name) {
    if (!file) return Promise.reject(new Error(t('file.choose', { name })));
    if (!/\.wav$/i.test(file.name) || file.size === 0 || file.size > 2 * 1024 * 1024) return Promise.reject(new Error(t('file.wav', { name })));
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () => reject(new Error(t('file.read_error', { name })));
      reader.onload = () => resolve(String(reader.result).split(',')[1]);
      reader.readAsDataURL(file);
    });
  }

  byId('voice-create-form').addEventListener('submit', (event) => {
    event.preventDefault();
    if (!reportWorkspaceValidity(event.target)) return;
    libraryAction(async () => {
      const provider = byId('library-provider').value;
      const clone = byId('voice-create-method').value === 'clone';
      const body = { provider, api_key_env: profileValue(provider, 'api_key_env'), name: byId('voice-create-name').value.trim() };
      if (clone) {
        body.reference_base64 = await wavBase64(byId('voice-create-reference').files[0], t("ui.the_reference_recording"));
        body.consent_base64 = provider === 'gemini' ? await wavBase64(byId('voice-create-consent').files[0], t("ui.the_consent_recording")) : '';
      } else {
        body.description = byId('voice-create-description').value.trim();
        body.language = byId('voice-create-language').value.trim();
      }
      const voice = await api(`/voices/${clone ? 'clone' : 'design'}`, { method: 'POST', body, timeout: 125000 });
      state.voices[provider] = [voice, ...state.voices[provider].filter(item => item.id !== voice.id)];
      byId('voice-create-reference').value = ''; byId('voice-create-consent').value = '';
      libraryMessage('notice', voice.kind === 'verification_required'
        ? t('voice.created_verification', { name: voice.name })
        : t('voice.created', { name: voice.name }));
    });
  });

  function renderInterfaceText() {
    const metadata = state.interface;
    if (!metadata) return;
    const select = byId('interface-language');
    select.value = metadata.language;
    const languageName = metadata.languages.find(item => item.code === i18n.language)?.name || i18n.language;
    byId('interface-language-status').textContent = t('interface.using', { language: languageName });
    // Update labels in place: drafts, native select values, focus, file uploads and
    // open dialogs stay attached to the same nodes during a language change.
    for (const option of document.querySelectorAll('[data-device-direction] option')) {
      const name = option.dataset.deviceName;
      option.textContent = !option.value ? t('ui.select_a_device')
        : option.dataset.deviceUnavailable === 'true' ? t('device.unavailable', { name })
        : option.dataset.deviceVirtual === 'true' ? t('device.virtual', { name }) : name;
    }
    for (const detail of document.querySelectorAll('[data-voice-kind]')) detail.textContent = `${voiceKind(detail.dataset.voiceKind)} · ${detail.dataset.voiceId}`;
    for (const link of document.querySelectorAll('a[href^="/help/"]')) {
      link.hreflang = 'pt';
      link.title = t('help.portuguese');
    }
    renderPlatform();
    renderFilePathPreview();
    if (state.config) {
      updateProviderControls(); updateGainLabels(); updateQualityHint();
      if (state.status) renderStatus(state.status);
      libraryControls();
      renderAutostartDescription();
      updateControls();
    }
  }

  async function applyInterface(metadata) {
    state.interface = metadata;
    const select = byId('interface-language');
    for (const item of metadata.languages) {
      if (![...select.options].some(option => option.value === item.code)) select.add(new Option(item.name, item.code));
    }
    await i18n.setLanguage(metadata.resolved_language);
    renderInterfaceText();
  }

  async function refreshInterface() { await applyInterface(await api('/interface')); }

  byId('interface-language').addEventListener('change', () => {
    const language = byId('interface-language').value;
    action(async () => {
      try {
        const metadata = await api('/interface', { method: 'PUT', body: { language }, revision: state.configRevision });
        state.config.interface = { ...state.config.interface, language: metadata.language };
        state.configRevision = String(metadata.config_revision);
        await applyInterface(metadata);
        announce(t('interface.saved'));
      } finally { byId('interface-language').value = state.interface?.language || state.config?.interface?.language || 'system'; }
    });
  });

  async function initialize() {
    try { await i18n.setLanguage('en'); }
    catch (error) { showError(error.message); return; }

    if (!token || !/^[a-f0-9]{64}$/.test(token)) {
      showError(t("ui.to_connect_securely_open_the_full_babel_dashboard_link_shown_in_the_termina"));
      byId('session-state').textContent = t("ui.dashboard_disconnected");
      updateControls();
      return;
    }
    try {
      const snapshot = await api('/config', { snapshot: true });
      state.configRevision = snapshot.revision;
      state.authenticated = true;
      applyConfig(snapshot.body);
      await refreshInterface();
      const results = await Promise.allSettled([refreshDevices(), api('/status'), refreshAutostart(), refreshPlatform()]);
      if (results[0].status === 'rejected') showError(results[0].reason.message);
      if (results[1].status === 'fulfilled') { renderStatus(results[1].value); await synchronizeConfig(results[1].value); }
      else showError(results[1].reason.message);
      if (results[2].status === 'rejected') byId('autostart-status').textContent = t('autostart.read_error', { error: results[2].reason.message });
      updateControls();
      refreshCredentialStatus(byId('profile-selector').value);
    } catch (error) {
      showError(error.message);
      byId('session-state').textContent = t("ui.dashboard_disconnected");
      updateControls();
    }
  }
  initialize();
  setInterval(pollStatus, 1000);
})();
