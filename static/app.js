// Progressive enhancement. Core forms and navigation work without JavaScript.
document.addEventListener('submit', (event) => {
  const form = event.target;
  if (form.dataset.confirm && !window.confirm(form.dataset.confirm)) event.preventDefault();
});
document.addEventListener('change', (event) => {
  if (event.target.matches('[data-select-all]')) {
    event.target.closest('form').querySelectorAll('input[name="ids[]"]').forEach(input => { input.checked = event.target.checked; });
  }
});
document.addEventListener('click', async (event) => {
  const remove = event.target.closest('[data-remove-split]');
  if (remove) { remove.closest('.split-row').remove(); return; }
  if (event.target.closest('[data-add-split]')) {
    const container = document.querySelector('#split-rows');
    const last = container?.querySelector('.split-row:last-child');
    if (!last) return;
    const clone = last.cloneNode(true);
    const index = Date.now();
    clone.querySelectorAll('[name]').forEach(input => { input.name = input.name.replace(/splits\[[^\]]+\]/, `splits[${index}]`); input.value = ''; });
    container.appendChild(clone); return;
  }
  const preview = event.target.closest('[data-preview-rule]');
  if (preview) {
    const form = preview.closest('form');
    const out = form.querySelector('[data-rule-preview]');
    const data = new FormData(form); data.set('preview', '1');
    preview.disabled = true; out.textContent = 'Checking matching history…';
    try {
      const response = await fetch(form.action, { method: 'POST', body: new URLSearchParams(data), headers: { Accept: 'application/json' } });
      if (!response.ok) throw new Error('Review the rule conditions and try again.');
      const result = await response.json(); out.replaceChildren();
      const heading = document.createElement('p'); heading.textContent = `${result.matches} matches; ${result.protected_categories} manual category choices will be preserved.`; out.appendChild(heading);
      const list = document.createElement('ul'); result.transactions.forEach(t => { const li = document.createElement('li'); li.textContent = `${t.date} · ${t.name} · ${t.amount} ${t.iso_currency_code}`; list.appendChild(li); }); out.appendChild(list);
    } catch (error) { out.textContent = error.message; } finally { preview.disabled = false; }
    return;
  }
  const connect = event.target.closest('[data-connect-bank]');
  if (connect) {
    const status = document.querySelector('[data-bank-status]'); connect.disabled = true; status.textContent = 'Opening secure bank connection…';
    const request = async (path, body) => {
      const response = await fetch(path, { method:'POST', headers:{'Content-Type':'application/json','X-CSRF-Token':document.querySelector('meta[name="csrf-token"]').content}, body: JSON.stringify(body) });
      const data = await response.json(); if (!response.ok) throw new Error(data.error || 'Bank connection failed.'); return data;
    };
    try {
      if (!window.Plaid) await new Promise((resolve,reject) => { const script = document.createElement('script'); script.src='https://cdn.plaid.com/link/v2/stable/link-initialize.js'; script.onload=resolve; script.onerror=()=>reject(new Error('Could not load Plaid.')); document.head.appendChild(script); });
      const {link_token} = await request('/api/v1/plaid/link_token',{});
      const handler = window.Plaid.create({token:link_token,onSuccess:async(public_token,metadata)=>{try {status.textContent='Saving your bank connection…';await request('/api/v1/plaid/exchange_public_token',{public_token,institution_id:metadata.institution?.institution_id,institution_name:metadata.institution?.name});window.location.assign('/');}catch(error){status.textContent=error.message;connect.disabled=false;}},onExit:(error)=>{status.textContent=error?.display_message||'Bank connection closed.';connect.disabled=false;handler.destroy();}});handler.open();
    } catch(error) { status.textContent=error.message; connect.disabled=false; }
  }
});
function responsiveAccounts(){const panel=document.querySelector('.accounts-panel');if(panel&&matchMedia('(max-width:700px)').matches)panel.open=false;}
document.addEventListener('DOMContentLoaded',responsiveAccounts);
document.addEventListener('turbo:load',responsiveAccounts);
