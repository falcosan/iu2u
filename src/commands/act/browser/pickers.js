(() => {
  const picks = window.__iu2uPicks = {};
  let last = 0;
  const ask = (kind, options) =>
    new Promise((resolve, reject) => {
      const id = ++last;
      picks[id] = { resolve, reject };
      __iu2uPick(JSON.stringify({ id, kind, multiple: !!(options && options.multiple) }));
    });
  window.showOpenFilePicker = options => ask('file', options);
  window.showDirectoryPicker = options => ask('directory', options);
})()
