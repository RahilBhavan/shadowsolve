import {chart,table} from './charts.js';

/** Walkthrough-first IA: readiness → candidates → route, then research panels. */
export const NAV_GROUPS = [
  {
    id: 'walkthrough',
    label: 'Walkthrough',
    sections: [
      ['tape', 'Readiness & tape'],
      ['candidates', 'Candidates'],
      ['route', 'Route'],
    ],
  },
  {
    id: 'research',
    label: 'Research',
    sections: [
      ['fragility', 'Fragility'],
      ['drift', 'Drift'],
      ['calibration', 'Calibration'],
      ['experiment', 'Experiment'],
      ['failures', 'Failures'],
    ],
  },
];

export const SECTIONS = NAV_GROUPS.flatMap((group) => group.sections);

const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const tone = (v) => {
  const s = String(v ?? 'unknown').toLowerCase();
  return /ok|good|online|accepted|healthy|ready|fresh|canonical/.test(s) ? 'good'
    : /unsafe|error|failed|rejected|bad/.test(s) ? 'bad'
    : /warn|stale|fallback|pending|unknown|unavailable/.test(s) ? 'warn'
    : 'active';
};
const status = (v) => `<span class="status" data-tone="${tone(v)}">${esc(v)}</span>`;
const title = (i, n, s) => `<header class="section-head"><div><p class="eyebrow">${String(i).padStart(2, '0')} / ${String(SECTIONS.length).padStart(2, '0')}</p><h1 id="section-title">${esc(n)}</h1><p class="section-summary">${esc(s)}</p></div></header>`;
const card = (h, b, extra = '') => `<article class="card${extra ? ` ${extra}` : ''}"><h2>${esc(h)}</h2>${b}</article>`;
const plain = (x) => x && typeof x === 'object' && !Array.isArray(x) && (Object.getPrototypeOf(x) === Object.prototype || Object.getPrototypeOf(x) === null);
const list = (x, defaults = {}) => (Array.isArray(x) ? x : []).filter(plain).map((v) => ({...defaults, ...v}));
const unavailable = (v) => v === undefined || v === null || v === '' || v === 'Unavailable' || (typeof v === 'string' && v.toLowerCase() === 'unavailable');
const hasRows = (rows) => Array.isArray(rows) && rows.length > 0;
const researchMissing = (label) => `<div class="status-banner" role="status"><strong>${esc(label)} unavailable in live projection.</strong><p>This panel is populated in fixture mode or offline research reports only. It is not fabricated from live solver state.</p></div>`;

function normalize(d = {}) {
  const calibration = plain(d.calibration) ? d.calibration : {};
  const poolDrift = plain(d.poolDrift) ? d.poolDrift : (unavailable(d.poolDrift) ? {} : {});
  const experiment = plain(d.experiment) ? d.experiment : {};
  return {
    ...d,
    auctions: list(d.auctions, {id: '', pair: '', block: '', status: 'unknown', value: 'Unavailable'}),
    candidates: list(d.candidates, {name: '', score: 'Unavailable', predictedValidity: 'Unavailable', decay: {}, decisionValue: 'Unavailable', explanation: ''}).map((x) => ({
      ...x,
      decay: plain(x.decay) ? {p50: 'Unavailable', p90: 'Unavailable', tailLoss: 'Unavailable', ...x.decay} : {p50: 'Unavailable', p90: 'Unavailable', tailLoss: 'Unavailable'},
    })),
    notifications: list(d.notifications, {auctionId: '', outcome: 'Unavailable', candidateId: 'Unavailable', message: ''}).map((x) => ({
      ...x,
      auctionId: x.auctionId ?? x.auction_id ?? 'Unavailable',
      candidateId: x.candidateId ?? x.candidate_id ?? 'Unavailable',
    })),
    horizons: list(d.horizons, {horizon: '', fragility: 'Unavailable', survival: 'Unavailable'}),
    failures: list(d.failures, {id: '', cause: '', components: '', counterfactual: ''}),
    calibration: {
      ...calibration,
      reliability: list(calibration.reliability, {bucket: '', predicted: 'Unavailable', observed: 'Unavailable', count: 'Unavailable'}),
      regimes: list(calibration.regimes, {regime: '', count: 'Unavailable', error: 'Unavailable'}),
    },
    poolDrift: {...poolDrift, rows: list(poolDrift.rows, {pool: '', reserveRatio: 'Unavailable', drift: 'Unavailable', replay: 'Unavailable'})},
    experiment,
    route: plain(d.route)
      ? {name: 'Unavailable', commitmentMargin: 'Unavailable', status: 'Unavailable', ...d.route, legs: Array.isArray(d.route.legs) ? d.route.legs.filter((x) => typeof x === 'string' || plain(x)) : []}
      : {name: 'Unavailable', commitmentMargin: 'Unavailable', status: 'Unavailable', legs: []},
    latestAdvisory: plain(d.latestAdvisory) ? d.latestAdvisory : null,
    meta: plain(d.meta) ? d.meta : {},
  };
}

function isFixture(d) {
  const source = String(d.meta?.source || '').toLowerCase();
  const mode = String(d.meta?.mode || d.meta?.persistenceMode || '').toLowerCase();
  return source === 'fixture' || mode === 'fixture' || mode.includes('fixture');
}

function banner(d) {
  const m = d.meta || {};
  const reasons = [];
  if (isFixture(d)) reasons.push('Fixture projection — not live chain data.');
  if (m.stale) reasons.push('Stale dashboard data.');
  if (String(m.chain?.status).toLowerCase() === 'unsafe') reasons.push(`Unsafe chain: ${m.chain.reason || 'chain health is unsafe'}.`);
  if (d.error || m.apiDegraded || m.apiError || m.degraded || m.error) reasons.push(`API degraded: ${d.error || m.apiError || m.error || 'data may be incomplete'}.`);
  if (m.modelFallback) reasons.push('Model fallback is active.');
  return reasons.length ? `<div class="status-banner" role="status">${esc(reasons.join(' '))}</div>` : '';
}

function provenanceBody(d) {
  const a = d.latestAdvisory;
  if (!plain(a)) return '<p>No advisory provenance reconstructed yet.</p>';
  return `<dl class="details-grid">
    <dt>Verification</dt><dd>${status(a.verification || 'Unavailable')}</dd>
    <dt>Network</dt><dd>${esc(a.sourceNetwork || a.source_network || 'Unavailable')}</dd>
    <dt>Block</dt><dd>${esc(a.sourceBlockNumber ?? a.block_number ?? 'Unavailable')}</dd>
    <dt>Hash</dt><dd><code>${esc(a.sourceBlockHash || a.block_hash || 'Unavailable')}</code></dd>
    <dt>Route</dt><dd>${esc(a.route || a.selectedRoute || 'Unavailable')}</dd>
    <dt>Gross out</dt><dd>${esc(a.grossOutput ?? a.gross_output ?? 'Unavailable')}</dd>
    <dt>Gas proxy / cost</dt><dd>${esc(a.gasProxy ?? a.gas_proxy ?? 'Unavailable')} / ${esc(a.gasCost ?? a.gas_cost ?? 'Unavailable')}</dd>
    <dt>Net surplus</dt><dd>${esc(a.netSurplus ?? a.signed_surplus ?? a.surplus ?? 'Unavailable')}</dd>
  </dl>`;
}

function tape(d) {
  const chain = d.meta.chain || {};
  const modelLabel = d.meta.modelFallback ? 'fallback' : 'online';
  return title(1, 'Readiness & tape', 'Can we trust this view? Then what did the solver observe?') +
    `<div class="readiness-row" role="group" aria-label="Service readiness">
      <div class="metric-tile"><span class="metric-label">Persistence</span><span class="metric-value">${status(d.meta.persistenceMode || 'unknown')}</span><span class="metric-detail">${esc(d.meta.persistenceStatus || 'unknown')} · rev ${esc(d.meta.revision ?? '—')}</span></div>
      <div class="metric-tile"><span class="metric-label">Model</span><span class="metric-value">${status(modelLabel)}</span><span class="metric-detail">${esc(d.meta.modelVersion || d.meta.modelSchemaHash || 'schema unknown')}</span></div>
      <div class="metric-tile"><span class="metric-label">Chain</span><span class="metric-value">${status(chain.status || 'unknown')}</span><span class="metric-detail">${esc(chain.reason || 'No chain status supplied.')}</span></div>
      <div class="metric-tile"><span class="metric-label">Source</span><span class="metric-value">${status(d.meta.source || 'unknown')}</span><span class="metric-detail">Finalized ${esc(chain.finalized ?? chain.finalizedHeight ?? '—')}${d.meta.stale ? ' · stale' : ''}</span></div>
    </div>
    <div class="grid">
      <section class="card span-12 panel-curve"><h2>Latest advisory provenance</h2>${provenanceBody(d)}</section>
      <section class="card span-8"><h2>Auctions</h2>${table(d.auctions, [['id', 'ID'], ['pair', 'Pair'], ['block', 'Block'], ['status', 'State'], ['value', 'Decision value']], 'Live auction tape')}</section>
      <section class="card span-4"><h2>Notifications</h2>${table(d.notifications, [['auctionId', 'Auction'], ['outcome', 'Outcome'], ['candidateId', 'Candidate'], ['message', 'Message']], 'Recent notification outcomes')}</section>
    </div>`;
}

function candidates(d) {
  const rows = d.candidates;
  return title(2, 'Candidates', 'Scores, validity, decay tails, and selection rationale.') +
    `<div class="table-wrap"><table><caption>Candidate comparison</caption><thead><tr><th scope="col">Candidate</th><th scope="col">Score</th><th scope="col">Validity</th><th scope="col">Decay p50 / p90</th><th scope="col">Tail loss</th><th scope="col">Decision value</th><th scope="col">Explanation</th></tr></thead><tbody>${
      rows.length
        ? rows.map((x) => `<tr><td>${esc(x.name)}</td><td>${esc(x.score)}</td><td>${esc(x.predictedValidity)}</td><td>${esc(x.decay.p50)} / ${esc(x.decay.p90)}</td><td>${esc(x.decay.tailLoss)}</td><td>${esc(x.decisionValue)}</td><td>${esc(x.explanation)}</td></tr>`).join('')
        : '<tr><td colspan="7">No candidate comparison data available.</td></tr>'
    }</tbody></table></div>
    <p class="callout">${d.policy?.eligible ? 'Eligible selection available.' : esc(d.policy?.reason || 'No eligibility policy result.')}</p>`;
}

function route(d) {
  return title(3, 'Route', 'Commitment path and margin without settlement controls.') +
    card(d.route.name || 'No route', `<p>Commitment margin: <strong>${esc(d.route.commitmentMargin)}</strong></p><p>Status: ${status(d.route.status || 'unknown')}</p>${table(d.route.legs.map((x, i) => ({leg: i + 1, route: typeof x === 'string' ? x : (x.name || x.pool || 'Unavailable')})), [['leg', 'Leg'], ['route', 'Route']], 'Route legs')}`, 'panel-curve');
}

function fragility(d) {
  const body = hasRows(d.horizons)
    ? chart(d.horizons.map((x) => ({label: `Horizon ${x.horizon}`, value: x.survival})), {label: 'Survival', value: 'value', summary: 'Survival probability by forecast horizon; see the table for exact values.'}) +
      table(d.horizons, [['horizon', 'Horizon'], ['fragility', 'Fragility'], ['survival', 'Survival']], 'Fragility and survival data')
    : researchMissing('Fragility and survival');
  return title(4, 'Fragility', 'Observed survival probabilities by forecast horizon.') + body;
}

function drift(d) {
  const body = hasRows(d.poolDrift.rows)
    ? `<p class="callout">${esc(d.poolDrift.unsupported || 'Tick/concentrated geometry unsupported; no fabricated result.')}</p>` +
      table(d.poolDrift.rows, [['pool', 'Pool'], ['reserveRatio', 'Reserve ratio'], ['drift', 'Drift'], ['replay', 'Replay']], 'Pool drift and replay')
    : researchMissing('Pool drift and replay');
  return title(5, 'Drift', 'Constant-product pool-state drift and replay outputs.') + body;
}

function calibration(d) {
  const body = (hasRows(d.calibration.reliability) || hasRows(d.calibration.regimes))
    ? table(d.calibration.reliability, [['bucket', 'Bucket'], ['predicted', 'Predicted'], ['observed', 'Observed'], ['count', 'Count']], 'Calibration reliability') +
      table(d.calibration.regimes, [['regime', 'Regime'], ['count', 'Count'], ['error', 'Error']], 'Regime table')
    : researchMissing('Calibration and regimes');
  return title(6, 'Calibration', 'Reliability buckets and regime-conditioned error.') + body;
}

function experiment(d) {
  const present = plain(d.experiment) && (d.experiment.name || d.experiment.baseline || d.experiment.shadow || d.experiment.effect || d.experiment.fieldsPresent);
  const body = present
    ? card(d.experiment?.name || 'Experiment', `<dl class="details-grid"><dt>Baseline</dt><dd>${esc(d.experiment?.baseline ?? 'Unavailable')}</dd><dt>Shadow</dt><dd>${esc(d.experiment?.shadow ?? 'Unavailable')}</dd><dt>Effect</dt><dd>${esc(d.experiment?.effect ?? 'Unavailable')}</dd><dt>Confidence interval</dt><dd>${esc(d.experiment?.confidenceInterval ?? 'Unavailable')}</dd></dl>`)
    : researchMissing('Paired baseline experiment');
  return title(7, 'Experiment', 'Paired baseline comparison — descriptive and read-only.') + body;
}

function failures(d) {
  return title(8, 'Failures', 'Root causes, components, and counterfactual outcomes.') +
    table(d.failures, [['id', 'ID'], ['cause', 'Root cause'], ['components', 'Components'], ['counterfactual', 'Counterfactual outcome']], 'Failure atlas');
}

const f = {tape, candidates, route, fragility, drift, calibration, experiment, failures};
export function renderSection(id, d) {
  const x = normalize(d);
  return banner(x) + f[id](x);
}
export {esc as escapeHtml, normalize, isFixture};
