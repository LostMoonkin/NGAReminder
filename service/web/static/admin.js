'use strict';

// 仅增强导航、筛选和表单交互；业务操作始终提交现有服务端表单。
document.documentElement.classList.add('js');
const one = (selector, root = document) => root.querySelector(selector);
let dirty = false;
document.addEventListener('input', event => {
  if (event.target.closest('form[method="post"]')) dirty = true;
});
document.addEventListener('change', event => {
  if (event.target.closest('form[method="post"]')) dirty = true;
});

document.querySelectorAll('[data-filter-group]').forEach(group => {
  let kind = 'all';
  const search = one('[data-search]', group);
  const apply = () => {
    const term = (search?.value || '').trim().toLocaleLowerCase();
    let count = 0;
    group.querySelectorAll('[data-filter-item]').forEach(item => {
      const matches = (kind === 'all' || item.dataset.kind === kind) &&
        (item.dataset.searchText || item.textContent).toLocaleLowerCase().includes(term);
      item.hidden = !matches;
      if (matches) count++;
    });
    group.querySelectorAll('.board-column').forEach(column => {
      const visible = column.querySelectorAll('[data-filter-item]:not([hidden])').length;
      one('[data-column-count]', column).textContent = visible;
      one('[data-column-empty]', column).hidden = visible > 0;
    });
    const result = one('[data-filter-result]', group);
    if (result) result.textContent = term || kind !== 'all' ? '找到 ' + count + ' 项' : '';
  };
  group.querySelectorAll('[data-filter]').forEach(button => {
    button.addEventListener('click', () => {
      kind = button.dataset.filter;
      group.querySelectorAll('[data-filter]').forEach(b => b.setAttribute('aria-pressed', String(b === button)));
      apply();
    });
  });
  search?.addEventListener('input', apply);
  apply();
});

document.querySelectorAll('[data-tabs]').forEach(container => {
  const nav = one('[data-tab-nav]', container);
  const links = [...nav.querySelectorAll('a')];
  const panels = [...container.querySelectorAll('[data-tab-panel]')];
  nav.setAttribute('role', 'tablist');
  const select = hash => {
    const active = links.find(link => link.hash === hash) || links[0];
    links.forEach(link => {
      link.setAttribute('role', 'tab');
      link.setAttribute('aria-selected', String(link === active));
      link.tabIndex = link === active ? 0 : -1;
    });
    panels.forEach(panel => {
      panel.setAttribute('role', 'tabpanel');
      panel.hidden = '#' + panel.id !== active.hash;
    });
  };
  links.forEach((link, i) => {
    link.addEventListener('click', event => {
      event.preventDefault();
      history.replaceState(null, '', link.hash);
      select(link.hash);
    });
    link.addEventListener('keydown', event => {
      if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? links.length - 1 :
        (i + (event.key === 'ArrowRight' ? 1 : links.length - 1)) % links.length;
      links[next].click();
      links[next].focus();
    });
  });
  select(location.hash);
  window.addEventListener('hashchange', () => select(location.hash));
});

const confirmation = one('#confirmation');
let pendingForm, pendingSubmitter;
document.addEventListener('submit', event => {
  const form = event.target;
  if (!form.matches('form[method="post"]')) return;
  if (form.dataset.confirm && form.dataset.confirmed !== 'true') {
    event.preventDefault();
    pendingForm = form;
    pendingSubmitter = event.submitter;
    one('[data-confirm-message]', confirmation).textContent = form.dataset.confirm;
    confirmation.showModal();
    return;
  }
  delete form.dataset.confirmed;
  if (form.dataset.submitting === 'true') {
    event.preventDefault();
    return;
  }
  form.dataset.submitting = 'true';
  form.setAttribute('aria-busy', 'true');
  // 延迟禁用按钮，保留本次原生表单提交及按钮参数。
  setTimeout(() => form.querySelectorAll('button[type="submit"], button:not([type])').forEach(b => b.disabled = true), 0);
});
one('[data-confirm-submit]', confirmation)?.addEventListener('click', () => {
  const form = pendingForm, submitter = pendingSubmitter;
  confirmation.close();
  if (!form?.isConnected) return;
  form.dataset.confirmed = 'true';
  if (submitter) form.requestSubmit(submitter);
  else form.requestSubmit();
});
document.querySelectorAll('[data-close-confirm]').forEach(b => b.addEventListener('click', () => confirmation.close()));
window.addEventListener('pageshow', event => {
  if (event.persisted) location.reload();
});
document.addEventListener('click', event => {
  const edit = event.target.closest('[data-feishu-edit], [data-feishu-cancel]');
  if (edit) {
    const form = edit.closest('[data-feishu-app]');
    const editing = edit.hasAttribute('data-feishu-edit');
    if (!editing) form.reset();
    one('fieldset', form).disabled = !editing;
    one('[data-feishu-edit]', form).hidden = editing;
    one('[data-feishu-save]', form).hidden = !editing;
    one('[data-feishu-save]', form).disabled = !editing;
    one('[data-feishu-cancel]', form).hidden = !editing;
    one('[data-feishu-hint]', form).textContent = editing ? '正在修改应用配置，保存后生效。' : '应用已保存，点击“修改”后才能编辑。';
    (editing ? form.elements.app_id : one('[data-feishu-edit]', form)).focus();
  }
  const add = event.target.closest('[data-add-rule]');
  if (add) {
    const group = add.closest('[data-rule-group]');
    const row = one('template', group).content.cloneNode(true);
    one('[data-rule-list]', group).append(row);
    dirty = true;
  }
  const remove = event.target.closest('[data-remove-rule]');
  if (remove) {
    remove.closest('[data-rule-row]').remove();
    dirty = true;
  }
});
if (one('[data-auto-refresh]')) {
  setInterval(() => {
    const editing = document.activeElement?.matches('input,textarea,select,[contenteditable]') ||
      location.hash === '#configuration';
    if (!dirty && !editing && !document.hidden && !confirmation?.open) location.reload();
  }, 5000);
}
if (location.pathname === '/admin') {
  if (location.hash === '#account') location.replace('/admin/settings');
  if (location.hash === '#saved-threads') location.replace('/admin/library');
}
