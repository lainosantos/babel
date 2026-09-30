'use strict';

// This is a visual explanation, not a connection to an audio or AI service.
const consolePreview = document.querySelector('.audio-console');
if (consolePreview) {
  for (const [index, wave] of [...document.querySelectorAll('.signal-wave')].entries()) {
    const fragment = document.createDocumentFragment();
    for (let sample = 0; sample < 28; sample += 1) {
      const bar = document.createElement('i');
      bar.style.height = `${7 + Math.abs(Math.sin(sample * 0.63 + index) * Math.cos(sample * 0.19)) * 38}px`;
      fragment.append(bar);
    }
    wave.append(fragment);
  }
  const buttons = [...document.querySelectorAll('.demo-switch')];
  for (const button of buttons) {
    button.addEventListener('click', () => {
      button.setAttribute('aria-pressed', String(button.getAttribute('aria-pressed') !== 'true'));
      const enabled = feature => buttons.find(item => item.dataset.feature === feature).getAttribute('aria-pressed') === 'true';
      const translate = enabled('translate');
      consolePreview.classList.toggle('passthrough', !translate);
      for (const label of document.querySelectorAll('.process-label')) label.textContent = translate ? 'Translate' : 'Original';
      document.querySelector('.outgoing .destination-label').textContent = translate ? 'Your voice in their language' : 'Your original voice';
      document.querySelector('.incoming .destination-label').textContent = translate ? 'Their voice in your language' : 'Their original audio';
      const descriptions = [translate ? 'Both directions translate.' : 'Original audio routes directly.'];
      if (enabled('transcribe')) descriptions.push('One original-language transcript.');
      if (enabled('record')) descriptions.push('One mixed recording.');
      if (!enabled('transcribe') && !enabled('record')) descriptions.push('Original audio stays available.');
      document.querySelector('#demo-status').textContent = descriptions.join(' ');
    });
  }
}
