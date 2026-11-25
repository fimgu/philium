import { fmt } from "./rpc.js";

/** Honest cost: a dollar figure only when something was actually priced. Shared by Home and Stats. */
export function costLabel(row) {
  const priced = row?.priced ?? 0;
  const included = row?.included ?? 0;
  if (priced > 0 && row.cost > 0) return fmt.money(row.cost);
  if (included > 0 && included >= (row.requests ?? 0)) return "On plan";
  if (included > 0) return "Mostly on plan";
  return "–";
}
