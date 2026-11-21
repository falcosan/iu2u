(() => {
  if (globalThis.__iu2uClock) return;
  const RealDate = Date,
    realSet = setTimeout,
    realClear = clearTimeout,
    realNow = Date.now,
    realPerformance = performance.now.bind(performance),
    timers = new Map();
  let offset = 0, last = 0, pump, running = false, frozen, read = false;
  const now = () => realNow() + offset,
    date = () => frozen === undefined ? now() : (read = true, frozen),
    tick = () => {
      if (read) {
        frozen += 1;
        read = false;
      }
    };
  const first = until => {
    let found;
    for (const timer of timers.values())
      if (timer.at <= until && (!found || timer.at < found.at)) found = timer;
    return found;
  };
  const fire = timer => {
    if (timer.every) timer.at += timer.every;
    else timers.delete(timer.id);
    try {
      typeof timer.run === 'function'
        ? Reflect.apply(timer.run, globalThis, timer.args)
        : (0, eval)(String(timer.run));
    } catch (error) {
      realSet(() => {
        throw error;
      });
    }
  };
  const schedule = () => {
    realClear(pump);
    const next = first(Infinity);
    if (next)
      pump = realSet(() => {
        const due = first(now());
        if (due) fire(due);
        schedule();
      }, next.at - now());
  };
  const add = (run, ms, args, repeat) => {
    const delay = Math.max(running ? 1 : 0, Number(ms) || 0);
    timers.set(++last, {
      id: last,
      run,
      args,
      at: now() + delay,
      every: repeat && Math.max(1, delay)
    });
    schedule();
    return last;
  };
  const clear = id => {
    if (timers.delete(id)) schedule();
  };
  const turn = () =>
    new Promise(resolve => {
      const channel = new MessageChannel();
      channel.port1.onmessage = resolve;
      channel.port2.postMessage(0);
    });
  const run = async ms => {
    const end = now() + ms;
    await Promise.all(
      Array.from(frames, frame => {
        try {
          return frame.__iu2uClock?.run(ms);
        } catch {}
      })
    );
    running = true;
    for (let due; (due = first(end)); await turn()) {
      offset += Math.max(0, due.at - now());
      fire(due);
    }
    running = false;
    offset += Math.max(0, end - now());
    schedule();
  };
  function FakeDate(...args) {
    return new.target
      ? Reflect.construct(RealDate, args.length ? args : [date()], new.target)
      : new RealDate(date()).toString();
  }
  FakeDate.prototype = RealDate.prototype;
  FakeDate.now = date;
  Object.setPrototypeOf(FakeDate, RealDate);
  performance.now = () => realPerformance() + offset;
  const stamp = Object.getOwnPropertyDescriptor(Event.prototype, 'timeStamp').get;
  Object.defineProperty(Event.prototype, 'timeStamp', {
    get() {
      return stamp.call(this) + offset;
    },
    configurable: true
  });
  Object.assign(globalThis, {
    Date: FakeDate,
    setTimeout: (run, ms, ...args) => add(run, ms, args, false),
    setInterval: (run, ms, ...args) => add(run, ms, args, true),
    clearTimeout: clear,
    clearInterval: clear
  });
  Object.defineProperty(globalThis, '__iu2uClock', {
    value: {
      run,
      freeze: at => {
        frozen = at;
        for (const key in globalThis)
          if (key.startsWith('on')) addEventListener(key.slice(2), tick, true);
      }
    }
  });
})()
