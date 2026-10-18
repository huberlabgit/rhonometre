// Embedded dashboards can resize without giving the parent access to river data.
(() => {
  let previous = 0;
  let pending = false;
  const report = () => {
    pending = false;
    const shell = document.querySelector('.app-shell.embed-mode');
    if (!shell) return;
    const height = Math.ceil(shell.getBoundingClientRect().height);
    if (height > 0 && height !== previous) {
      previous = height;
      parent.postMessage({type: 'rhonometre:resize', height}, '*');
    }
  };
  const schedule = () => {
    if (!pending) { pending = true; requestAnimationFrame(report); }
  };
  new ResizeObserver(schedule).observe(document.body);
  new MutationObserver(schedule).observe(document.body, {childList: true, subtree: true});
  schedule();
})();
