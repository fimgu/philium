// Which publish step is next for a worktree, from `workspace.publish_state`.
export function publishSteps(state, picked = []) {
  const files = state?.files || [];
  const ahead = state?.ahead == null ? null : Number(state.ahead);
  return {
    canCommit: picked.length > 0 && picked.every((p) => files.some((f) => f.path === p)),
    canPush: !!state?.branch && !!state?.remote && (ahead === null || ahead > 0),
    canPr: !!state?.branch && !!state?.remote && ahead === 0,
  };
}
