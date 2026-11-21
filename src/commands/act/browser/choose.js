function(label) {
  const words = text => text.trim().split(/\s+/).join(' ');
  const option = [...this.options].find(
    o => !o.disabled && (words(o.label) === label || words(o.text) === label)
  );
  if (!option) return false;
  option.selected = true;
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return true;
}
