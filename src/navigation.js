import {SECTIONS} from './render.js';

export function selectSection(hash, stored) {
  const valid = value => SECTIONS.some(([id]) => id === value);
  return valid(hash) ? hash : valid(stored) ? stored : 'tape';
}
