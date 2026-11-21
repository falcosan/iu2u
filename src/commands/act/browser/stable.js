function(within, still, apart) {
  return new Promise(done => {
    const chain = new Set();
    for (let node = this; node; node = node.parentNode ?? node.host) chain.add(node);
    let last = null, since = 0;
    const frame = time => {
      if (last !== null && time - since < apart) {
        requestAnimationFrame(frame);
        return;
      }
      const box = this.getBoundingClientRect();
      const at = [box.x + box.width / 2, box.y + box.height / 2];
      if (last !== null) {
        done(
          Math.hypot(at[0] - last[0], at[1] - last[1]) < still &&
            !document.getAnimations().some(
              animation =>
                animation.pending &&
                animation.timeline?.currentTime != null &&
                chain.has(animation.effect?.target)
            )
        );
        return;
      }
      last = at;
      since = time;
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
    setTimeout(() => done(null), within);
  });
}
