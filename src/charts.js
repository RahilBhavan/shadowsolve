const n=(v,range)=>{const x=Number(v);if(range!==undefined){const min=Number(range?.[0]),max=Number(range?.[1]);if(!Number.isFinite(min)||!Number.isFinite(max)||min>max||!Number.isFinite(x)||x<min||x>max)return null}return Number.isFinite(x)?Math.max(range?.[0]??-Infinity,Math.min(range?.[1]??Infinity,x)):null};const t=v=>String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
let chartSeq=0;
export function chart(rows,{label='Value',value='value',summary,range}={}){
  const safe=(Array.isArray(rows)?rows:[]).filter(r=>r&&typeof r==='object'&&!Array.isArray(r)).map(r=>({label:t(r.label),value:n(r[value],range)}));
  const valid=safe.filter(r=>r.value!==null);
  const max=Math.max(1,...valid.map(r=>r.value));
  const unavailable=safe.filter(r=>r.value===null).length;
  const id=`chart-summary-${++chartSeq}`;
  const summaryText=t((summary||`${label}: ${safe.length} values shown.`)+(unavailable?` ${unavailable} unavailable.`:''));
  return `<figure class="chart-block"><figcaption class="chart-summary" id="${id}">${summaryText}</figcaption><div class="chart chart-animate" aria-hidden="true">${safe.map(r=>{const bucket=r.value===null?'invalid':Math.round((r.value||0)/max*10)*10;return `<div class="bar bar-${bucket}"><span>${r.label}</span><b>${r.value===null?'Unavailable':r.value}</b></div>`}).join('')}</div><p class="sr-only">Exact values are in the table that follows.</p></figure>`;
}
export function table(rows,columns,caption){const list=(Array.isArray(rows)?rows:[]).filter(r=>r&&typeof r==='object'&&!Array.isArray(r));const empty=`<tr><td colspan="${columns.length}">${t(`No ${caption.toLowerCase()} data available.`)}</td></tr>`;return `<div class="table-wrap"><table><caption>${t(caption)}</caption><thead><tr>${columns.map(c=>`<th scope="col">${t(c[1])}</th>`).join('')}</tr></thead><tbody>${list.length?list.map(r=>`<tr>${columns.map(c=>`<td>${t(r[c[0]])}</td>`).join('')}</tr>`).join(''):empty}</tbody></table></div>`}
export {t as escapeChartText};
