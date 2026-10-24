import {fetchDashboard} from './api.js';
import {renderSection, SECTIONS, NAV_GROUPS, escapeHtml} from './render.js';
import {selectSection} from './navigation.js';

const content = document.querySelector('#section-content');
const main = document.querySelector('#content');
const live = document.querySelector('#live-region');
const nav = document.querySelector('#nav-list');
const mobileNav = document.querySelector('#mobile-nav');
const storageKey = 'shadowsolve-selected-section';

function announce(message) {
  live.textContent = message;
}

function storedSection() {
  try { return localStorage.getItem(storageKey); } catch { return null; }
}

function choose(hash = location.hash.slice(1), {focus = false} = {}) {
  const id = selectSection(hash, storedSection());
  try { localStorage.setItem(storageKey, id); } catch { /* preference is optional */ }
  if (location.hash.slice(1) !== id) history.replaceState(null, '', `#${id}`);
  [...nav.querySelectorAll('button')].forEach((button) => {
    const active = button.dataset.section === id;
    button.classList.toggle('active', active);
    if (active) button.setAttribute('aria-current', 'page');
    else button.removeAttribute('aria-current');
  });
  mobileNav.value = id;
  if (window.dashboard) {
    content.innerHTML = renderSection(id, window.dashboard);
    content.classList.remove('section-enter');
    void content.offsetWidth;
    content.classList.add('section-enter');
    if (focus) content.focus({preventScroll: true});
    announce(`${SECTIONS.find(([section]) => section === id)[1]} loaded.`);
  }
}

let index = 0;
NAV_GROUPS.forEach((group) => {
  const wrap = document.createElement('div');
  wrap.className = 'nav-group';
  wrap.dataset.group = group.id;
  const label = document.createElement('p');
  label.className = 'nav-group-label';
  label.id = `nav-group-${group.id}`;
  label.textContent = group.label;
  const list = document.createElement('ul');
  list.setAttribute('aria-labelledby', label.id);
  group.sections.forEach(([id, sectionLabel]) => {
    index += 1;
    const item = document.createElement('li');
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'nav-button';
    button.dataset.section = id;
    button.innerHTML = `<span class="nav-index" aria-hidden="true">${String(index).padStart(2, '0')}</span><span>${escapeHtml(sectionLabel)}</span>`;
    button.addEventListener('click', () => {
      if (location.hash.slice(1) !== id) location.hash = id;
      else choose(id, {focus: true});
    });
    item.append(button);
    list.append(item);
    const option = document.createElement('option');
    option.value = id;
    option.textContent = `${String(index).padStart(2, '0')} · ${sectionLabel}`;
    mobileNav.append(option);
  });
  wrap.append(label, list);
  nav.append(wrap);
});

nav.addEventListener('keydown', (event) => {
  const buttons = [...nav.querySelectorAll('.nav-button')];
  const current = buttons.indexOf(document.activeElement);
  if (current < 0) return;
  const map = {ArrowDown: current + 1, ArrowUp: current - 1, Home: 0, End: buttons.length - 1};
  if (!(event.key in map)) return;
  event.preventDefault();
  buttons[Math.max(0, Math.min(buttons.length - 1, map[event.key]))].focus();
});

mobileNav.addEventListener('change', () => {
  const id = mobileNav.value;
  if (location.hash.slice(1) !== id) location.hash = id;
  else choose(id, {focus: true});
});
window.addEventListener('hashchange', () => choose(undefined, {focus: true}));

let pending = false;
async function load() {
  if (pending) return;
  pending = true;
  content.innerHTML = '<div class="loading" role="status" aria-label="Loading dashboard"><span></span><span></span><span></span></div>';
  announce('Loading dashboard.');
  const result = await fetchDashboard();
  pending = false;
  if (result.error) {
    content.innerHTML = `<div class="status-banner error-state" role="alert"><strong>Dashboard unavailable.</strong><p>${escapeHtml(result.error)}</p><button class="button secondary" type="button" id="retry">Retry</button></div>`;
    document.querySelector('#retry').addEventListener('click', load);
    announce(`Dashboard failed to load: ${result.error}`);
  } else {
    window.dashboard = {...result.data, meta: {...result.data.meta, stale: result.stale, source: result.source}};
    choose();
  }
}
load();
