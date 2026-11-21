({ left, top, width, height, columns, rows, labels, scale }) => {
  document.getElementById('__iu2u_grid')?.remove();
  const host = document.createElement('div');
  host.id = '__iu2u_grid';
  host.popover = 'manual';
  host.style.cssText = `all: initial; position: fixed; left: ${left}px; top: ${top}px; width: ${width}px; height: ${height}px; z-index: 2147483647; pointer-events: none`;
  const root = host.attachShadow({ mode: 'closed' });
  const [wide, high] = [width / columns, height / rows];
  const size = Math.min(18, Math.max(11, Math.min(wide, high) * scale * 0.16)) / scale;
  const drawing = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(drawing, 'svg');
  svg.setAttribute('width', width);
  svg.setAttribute('height', height);
  svg.style.cssText = 'position: absolute; left: 0; top: 0';
  const lines = document.createElementNS(drawing, 'path');
  lines.setAttribute(
    'd',
    [...Array(columns + 1).keys()]
      .map(column => `M${column * wide} 0V${height}`)
      .concat([...Array(rows + 1).keys()].map(row => `M0 ${row * high}H${width}`))
      .join('')
  );
  lines.setAttribute('stroke', 'rgba(255, 0, 255, 0.75)');
  lines.setAttribute('stroke-width', 1 / scale);
  lines.setAttribute('fill', 'none');
  svg.append(lines);
  root.append(svg);
  labels.forEach((label, cell) => {
    const tag = document.createElement('span');
    tag.textContent = label;
    tag.style.cssText = `position: absolute; left: ${(cell % columns) * wide}px; top: ${Math.floor(cell / columns) * high}px; font: bold ${size}px/1 sans-serif; color: #fff; background: rgba(0, 0, 0, 0.55); padding: ${1 / scale}px ${3 / scale}px`;
    root.append(tag);
  });
  document.documentElement.append(host);
  host.showPopover?.();
  return true;
}
