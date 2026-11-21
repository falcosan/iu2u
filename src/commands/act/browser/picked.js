(id, tree, many) => {
  const pick = window.__iu2uPicks[id];
  delete window.__iu2uPicks[id];
  if (!tree) {
    pick.reject(new DOMException('The user aborted a request.', 'AbortError'));
    return;
  }
  const bytes = text => Uint8Array.from(atob(text), c => c.charCodeAt(0));
  const like = (handle, kind) => {
    const type = globalThis[kind];
    if (type) Object.setPrototypeOf(handle, type.prototype);
    return handle;
  };
  const common = {
    queryPermission: async () => 'granted',
    requestPermission: async () => 'granted'
  };
  const file = node => {
    const blob = new File([bytes(node.data)], node.name, { type: node.type });
    const handle = {
      ...common,
      kind: 'file',
      name: node.name,
      getFile: async () => blob,
      isSameEntry: async other => other === handle
    };
    return like(handle, 'FileSystemFileHandle');
  };
  const directory = node => {
    const children = new Map(
      node.children.map(child => [child.name, child.children ? directory(child) : file(child)])
    );
    const get = kind => async name => {
      const child = children.get(name);
      if (!child || child.kind !== kind)
        throw new DOMException(
          name + ' was not found',
          child ? 'TypeMismatchError' : 'NotFoundError'
        );
      return child;
    };
    const handle = {
      ...common,
      kind: 'directory',
      name: node.name,
      entries: async function* () {
        yield* children.entries();
      },
      keys: async function* () {
        yield* children.keys();
      },
      values: async function* () {
        yield* children.values();
      },
      [Symbol.asyncIterator]() {
        return this.entries();
      },
      getFileHandle: get('file'),
      getDirectoryHandle: get('directory'),
      isSameEntry: async other => other === handle
    };
    return like(handle, 'FileSystemDirectoryHandle');
  };
  pick.resolve(
    tree.children ? directory(tree) : many ? tree.files.map(file) : [file(tree.files[0])]
  );
}
