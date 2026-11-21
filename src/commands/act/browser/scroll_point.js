(() => {
  const shown = e => {
    const r = e.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0 &&
      r.top < innerHeight && r.left < innerWidth;
  };
  const scrolls = [...document.querySelectorAll('*')].filter(
    e =>
      e.scrollHeight - e.scrollTop - e.clientHeight > 1 &&
      ['auto', 'scroll', 'overlay'].includes(getComputedStyle(e).overflowY) &&
      shown(e)
  );
  const layered = scrolls.filter(e =>
    e.closest('dialog[open], [role=dialog], [role=alertdialog], [role=listbox], [role=menu], [popover]')
  );
  const area = e => e.clientWidth * e.clientHeight;
  const center = e => {
    const r = e.getBoundingClientRect();
    const left = Math.max(0, r.left), top = Math.max(0, r.top);
    return [
      (left + Math.min(innerWidth, r.right)) / 2,
      (top + Math.min(innerHeight, r.bottom)) / 2
    ];
  };
  const open = layered.pop();
  if (open) return [...center(open), true];
  const page = document.scrollingElement;
  const locked = [document.documentElement, document.body].some(
    e => e && ['hidden', 'clip'].includes(getComputedStyle(e).overflowY)
  );
  if (page && !locked && page.scrollHeight - scrollY - innerHeight > 1)
    return [innerWidth / 2, innerHeight / 2, true];
  const pick = scrolls.sort((a, b) => area(b) - area(a))[0];
  if (pick) return [...center(pick), true];
  const frame = [...document.querySelectorAll('iframe')]
    .filter(f => {
      const inner = f.contentDocument?.scrollingElement;
      return shown(f) &&
        (!inner || inner.scrollHeight - inner.scrollTop - inner.clientHeight > 1);
    })
    .sort((a, b) => area(b) - area(a))[0];
  return frame ? [...center(frame), Boolean(frame.contentDocument)] : null;
})()
