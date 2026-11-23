// Include configured providers without traffic and keep historical usage after removal.
export function providerUsageRows(providers, usage) {
  const remaining = new Map(usage.map((row) => [row.provider_id, row]));
  const rows = providers.map((provider) => {
    const stats = remaining.get(provider.id);
    remaining.delete(provider.id);
    return { id: provider.id, provider, stats };
  });
  for (const [id, stats] of remaining) rows.push({ id, stats });
  return rows;
}
