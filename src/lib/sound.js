// Subtle Web Audio API chimes for agent state transitions (no external audio files needed).
import { createSignal } from "solid-js";

const [soundEnabled, setSoundEnabledRaw] = createSignal(localStorage.getItem("philium.sound") !== "off");
export { soundEnabled };

export function setSoundEnabled(on) {
  const next = !!on;
  setSoundEnabledRaw(next);
  localStorage.setItem("philium.sound", next ? "on" : "off");
}

let audioCtx = null;
function ctx() {
  if (typeof window === "undefined") return null;
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return null;
  if (!audioCtx) audioCtx = new AC();
  if (audioCtx.state === "suspended") audioCtx.resume().catch(() => {});
  return audioCtx;
}

function tone(ac, freq, startOffset, duration, gainPeak = 0.06, type = "sine") {
  const now = ac.currentTime + startOffset;
  const osc = ac.createOscillator();
  const gain = ac.createGain();
  osc.type = type;
  osc.frequency.setValueAtTime(freq, now);
  gain.gain.setValueAtTime(0.0001, now);
  gain.gain.exponentialRampToValueAtTime(gainPeak, now + 0.015);
  gain.gain.exponentialRampToValueAtTime(0.0001, now + duration);
  osc.connect(gain);
  gain.connect(ac.destination);
  osc.start(now);
  osc.stop(now + duration + 0.02);
}

export function playSound(kind) {
  if (!soundEnabled()) return;
  const ac = ctx();
  if (!ac) return;
  try {
    if (kind === "waiting") {
      // Gentle two-note attention chime (A4 -> E5)
      tone(ac, 440, 0, 0.14, 0.055);
      tone(ac, 659.25, 0.11, 0.22, 0.06);
    } else if (kind === "done") {
      // Soft completion triad (C5 -> E5 -> G5)
      tone(ac, 523.25, 0, 0.12, 0.045);
      tone(ac, 659.25, 0.08, 0.14, 0.045);
      tone(ac, 783.99, 0.16, 0.24, 0.05);
    } else if (kind === "failed") {
      // Low two-note alert (E4 -> C4)
      tone(ac, 329.63, 0, 0.14, 0.055, "triangle");
      tone(ac, 261.63, 0.12, 0.24, 0.06, "triangle");
    }
  } catch {
    // Ignore audio context restrictions before first user gesture
  }
}
