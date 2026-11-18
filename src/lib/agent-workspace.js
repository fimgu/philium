// An explicit repository identity wins over legacy name/path hints.
export function workspaceAgents(agents, repoId, repos = []) {
  if (!repoId) return agents;
  const repo = repos.find(item => item.id === repoId);
  const uniqueName = repo?.name && repos.filter(r => r.name === repo.name).length === 1;
  return agents.filter(agent => {
    if (agent.repo_id) return agent.repo_id === repoId;
    if (agent.cwd) {
      // A nested registered checkout wins over its enclosing checkout.
      let owner, longest = 0;
      for (const r of repos) {
        const p = r.path?.replace(/\/+$/, "");
        if (!p || !(agent.cwd === p || agent.cwd.startsWith(`${p}/`))) continue;
        if (p.length === longest) owner = undefined;
        if (p.length > longest) { longest = p.length; owner = r.id; }
      }
      return owner === repoId;
    }
    return Boolean(uniqueName && agent.repo_name === repo.name);
  });
}

export function selectedAgentId(agents, requestedId) {
  return agents.find(agent => agent.id === requestedId)?.id ?? agents[0]?.id;
}

export function spawnRepoDefault(preset, activeRepoId) {
  return preset?.repo_id ?? activeRepoId ?? "";
}
