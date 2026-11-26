// Inline stroke icons (no icon font, no network). 16px, currentColor.
const P = {
  home: "M3 10.5 10 4l7 6.5V17a1 1 0 0 1-1 1h-4v-5H8v5H4a1 1 0 0 1-1-1z",
  agents: "M7 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm6.5 0a2.5 2.5 0 1 0 0-5M2 17c0-2.8 2.2-5 5-5s5 2.2 5 5m2-4.8c2 .4 4 2 4 4.8",
  board: "M3 4h4v12H3zM8.5 4h4v8h-4zM14 4h3v5h-3z",
  chat: "M4 4h12a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H9l-4 3v-3H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1Z",
  terminal: "M3 4h14v12H3zM6 8l2.5 2L6 12m4 0h4",
  globe: "M10 18a8 8 0 1 0 0-16 8 8 0 0 0 0 16ZM2 10h16M10 2c2.2 2.4 3 5 3 8s-.8 5.6-3 8c-2.2-2.4-3-5-3-8s.8-5.6 3-8Z",
  repo: "M5 3h10v14l-5-3-5 3zM8 7h4",
  activity: "M2 10h4l2-6 4 12 2-6h4",
  stats: "M4 16V9m4 7V5m4 11v-5m4 5V3",
  key: "M12.5 7.5a3.5 3.5 0 1 1-7 0 3.5 3.5 0 0 1 7 0ZM11 10l6 6m-3-3 2-2",
  settings: "M10 12.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5ZM10 2v2m0 12v2m8-8h-2M4 10H2m13.7-5.7-1.4 1.4M5.7 14.3l-1.4 1.4m11.4 0-1.4-1.4M5.7 5.7 4.3 4.3",
  spark: "M10 2l1.8 5.2L17 9l-5.2 1.8L10 16l-1.8-5.2L3 9l5.2-1.8z",
  search: "M9 15A6 6 0 1 0 9 3a6 6 0 0 0 0 12Zm4.5-1.5L17 17",
  shield: "M10 2 4 4.5v5c0 3.6 2.6 6.9 6 8 3.4-1.1 6-4.4 6-8v-5z",
  plus: "M10 4v12M4 10h12",
  play: "M6 4l10 6-10 6z",
  stop: "M5 5h10v10H5z",
  trash: "M4 6h12M8 6V4h4v2m-6 0 1 11h6l1-11",
  branch: "M6 3v10m0 0a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm8-6a2 2 0 1 0 0-4 2 2 0 0 0 0 4Zm0 0c0 3-3 4-8 6",
  mail: "M3 5h14v10H3zM3 5l7 6 7-6",
  graph: "M5 5a2 2 0 1 0 0 .01M15 5a2 2 0 1 0 0 .01M10 15a2 2 0 1 0 0 .01M6.5 6.5 9 13m4.5-6.5L11 13M7 5h6",
  lock: "M5 9h10v8H5zM7 9V6a3 3 0 0 1 6 0v3",
  refresh: "M16 10a6 6 0 1 1-2-4.5M16 3v4h-4",
  external: "M11 3h6v6m0-6-8 8M15 12v4H4V5h4",
  image: "M3 4h14v12H3zM3 13l4-4 4 4 2-2 4 4M13 8a1 1 0 1 0 0 .01",
  camera: "M3 6h3l1.5-2h5L14 6h3v10H3zM10 13.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Z",
  send: "M10 16V4m0 0-5 5m5-5 5 5",
  pr: "M6 3v14M6 3a2 2 0 1 0 0 .01M14 17a2 2 0 1 0 0 .01M14 15V8a2 2 0 0 0-2-2H9",
  grid: "M3 3h6v6H3zM11 3h6v6h-6zM3 11h6v6H3zM11 11h6v6h-6z",
  brain: "M7.5 4a3 3 0 0 0-3 3c0 .8.3 1.5.8 2A3 3 0 0 0 4 11.5a3 3 0 0 0 3 3h1V4zm5 0a3 3 0 0 1 3 3c0 .8-.3 1.5-.8 2a3 3 0 0 1 1.3 2.5 3 3 0 0 1-3 3h-1V4zM10 4v12",
  layers: "M10 3 3 7l7 4 7-4-7-4zM3 11l7 4 7-4M3 15l7 4 7-4",
  check: "M4.5 10.5 8.2 14.2 15.5 6.5",
  chevronDown: "M5.5 7.5 10 12l4.5-4.5",
  chevronRight: "M7.5 5.5 12 10l-4.5 4.5",
  volume: "M3 8.5h3l4-3.5v10l-4-3.5H3zM13.5 7.5a3.5 3.5 0 0 1 0 5M15.8 5.2a6.5 6.5 0 0 1 0 9.6",
  volumeOff: "M3 8.5h3l4-3.5v10l-4-3.5H3zM13.5 8l4 4m0-4-4 4",
  keyboard: "M3 6h14a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1Zm3 3h.01M9 9h.01M12 9h.01M15 9h.01M6.5 12h7",
  command: "M7 7a2 2 0 1 1 2-2v10a2 2 0 1 1-2-2h6a2 2 0 1 1-2 2V5a2 2 0 1 1 2 2H7Z",
  more: "M5 10a1 1 0 1 0 0 .01M10 10a1 1 0 1 0 0 .01M15 10a1 1 0 1 0 0 .01",
  sidebar: "M3 4h14a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1Zm5 0v12",
  sun: "M10 13a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm0-10v2m0 10v2m7-7h-2M5 10H3m11.95-4.95-1.41 1.41M6.46 13.54l-1.41 1.41m9.9 0-1.41-1.41M6.46 6.46 5.05 5.05",
  moon: "M15.5 11.8A6 6 0 0 1 8.2 4.5a6 6 0 1 0 7.3 7.3Z",
  x: "M5 5l10 10M15 5 5 15",
  alert: "M10 3 2.5 16.5h15L10 3Zm0 5v4m0 2.5h.01",
  info: "M10 18a8 8 0 1 0 0-16 8 8 0 0 0 0 16Zm0-9v5m0-7.5h.01",
};

export function Icon(props) {
  return (
    <svg width={props.size ?? 16} height={props.size ?? 16} viewBox="0 0 20 20" fill="none"
      stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" style={{ flex: "none" }}>
      <path d={P[props.name] ?? P.spark} />
    </svg>
  );
}
