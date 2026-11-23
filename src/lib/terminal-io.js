// xterm's onData combines user input and parser-generated replies (OSC, DA,
// DSR, etc.). Keep it disconnected from the PTY while parsing old output,
// not just while enqueueing that output with the asynchronous write API.
export function terminalIO(term, canSend, send) {
  let replaying = false;
  let closed = false;
  const input = term.onData((data) => {
    if (!closed && !replaying && canSend()) send(data);
  });

  return {
    write(data, { replay = false, done } = {}) {
      if (closed) return;
      const parsed = () => {
        if (!closed) done?.();
      };
      if (!replay || !data.length) {
        term.write(data, parsed);
        return;
      }
      // These callbacks run in xterm's FIFO parse order. The empty write
      // starts the gate after any earlier live output has finished parsing.
      term.write(new Uint8Array(0), () => { replaying = true; });
      term.write(data, () => {
        replaying = false;
        parsed();
      });
    },
    dispose() {
      closed = true;
      input.dispose();
    },
  };
}
