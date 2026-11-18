// Offline only: no kubeconfig, provider, credentials or cluster client.
export function tenantBundle(id) {
  if (typeof id !== 'string' || !/^[a-z0-9][a-z0-9-]{0,49}$/.test(id) || id.endsWith('-')) throw Error('Invalid checkout ID');
  const ns = `philium-${id}`;
  const obj = (apiVersion, kind, name, properties) => ({apiVersion, kind, metadata:{name, namespace:ns}, ...properties});
  return {apiVersion:'v1', kind:'List', items:[
    {apiVersion:'v1',kind:'Namespace',metadata:{name:ns,labels:{'app.kubernetes.io/managed-by':'philium','philium.dev/checkout-id':id,'pod-security.kubernetes.io/enforce':'restricted','pod-security.kubernetes.io/audit':'restricted','pod-security.kubernetes.io/warn':'restricted'}}},
    obj('networking.k8s.io/v1','NetworkPolicy','deny-by-default',{spec:{podSelector:{},policyTypes:['Ingress','Egress'],ingress:[],egress:[]}}),
    obj('v1','ResourceQuota','budget',{spec:{hard:{pods:'4','count/jobs.batch':'4','requests.cpu':'1','limits.cpu':'2','requests.memory':'1Gi','limits.memory':'2Gi','limits.ephemeral-storage':'2Gi',persistentvolumeclaims:'0','services.loadbalancers':'0','services.nodeports':'0'}}}),
    obj('v1','LimitRange','container-defaults',{spec:{limits:[{type:'Container',defaultRequest:{cpu:'100m',memory:'128Mi','ephemeral-storage':'64Mi'},default:{cpu:'500m',memory:'512Mi','ephemeral-storage':'512Mi'}}]}}),
    ...['default','check-workload','check-observer'].map(name=>obj('v1','ServiceAccount',name,{automountServiceAccountToken:false})),
    obj('rbac.authorization.k8s.io/v1','Role','check-observer',{rules:[{apiGroups:['batch'],resources:['jobs'],verbs:['get','list','watch']},{apiGroups:[''],resources:['pods'],verbs:['get','list','watch']}]}),
    obj('rbac.authorization.k8s.io/v1','RoleBinding','check-observer',{roleRef:{apiGroup:'rbac.authorization.k8s.io',kind:'Role',name:'check-observer'},subjects:[{kind:'ServiceAccount',namespace:ns,name:'check-observer'}]})
  ]};
}
