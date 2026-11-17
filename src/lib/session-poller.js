// Each selection owns its cursor, request and retry timer. Transport requests
// cannot be cancelled, so retired owners must never publish or schedule work.
export function createSessionPoller({ fetchEvents, onEvents, isBusy, schedule = setTimeout, cancel = clearTimeout }) {
  let owner;
  function retire() {
    if (!owner) return;
    owner.active = false;
    cancel(owner.timer);
    owner = undefined;
  }
  async function run(current) {
    if (!current?.active || current.pending) return;
    cancel(current.timer);
    current.pending = true;
    try {
      const events = await fetchEvents(current.id, current.after);
      if (!current.active) return;
      if (events?.length) {
        current.after = events.at(-1).seq;
        onEvents(events);
      }
    } catch { /* desk restarting */ }
    finally {
      current.pending = false;
      if (current.active && isBusy()) current.timer = schedule(() => run(current), 400);
    }
  }
  return {
    select(id) {
      retire();
      if (id) {
        owner = { id, after: 0, active: true, pending: false };
        return run(owner);
      }
    },
    poll(id) { if (owner?.id === id) return run(owner); },
    dispose: retire,
  };
}
