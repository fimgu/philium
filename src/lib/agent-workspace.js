// An explicit repository identity wins over legacy name/path hints.
export function workspaceAgents(agents, repoId, repos = []) {
  if (!repoId) return agents;
  const repo = repos.find(item => item.id === repoId);
  const path = repo?.path?.replace(/\/+$/, "");
  return agents.filter(agent => {
    if (agent.repo_id) return agent.repo_id === repoId;
    return Boolean((repo?.name && agent.repo_name === repo.name)
      || (path && (agent.cwd === path || agent.cwd?.startsWith(`${path}/`))));
  });
}

export function selectedAgentId(agents, requestedId) {
  return agents.find(agent => agent.id === requestedId)?.id ?? agents[0]?.id;
}

export function spawnRepoDefault(preset, activeRepoId) {
  return preset?.repo_id ?? activeRepoId ?? "";
}
