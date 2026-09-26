const df = dd.dataflow(scope => { // @code:build
  const membership = scope.input<[person: number, team: number]>() // @code:input +2
  const permission = scope.input<[team: number, resource: number]>()
  const grant      = scope.input<[person: number, resource: number]>()
  const viaTeams = membership.rows.pipe(
    keyBy(([person, team]) => team), // @code:keyby
    join(permission.rows.pipe(keyBy(([team, resource]) => team))), // @code:join @code:keyby
    map(([team, [person], [, resource]]) => [person, resource] as const), // @code:map
  )
  const access = merge(viaTeams, grant.rows).pipe(distinct()) // @code:union @code:distinct
  return { membership, permission, grant, access$: access.changes$ } // @code:changes
}) // @code:build
df.access$.subscribe(({ row, time, diff }) => render(row, diff)) // @code:subscribe
df.membership.update([2, 10], +1) // @code:update +1
df.permission.update([10, 200], +1)
for (const i of [df.membership, df.permission, df.grant]) i.advanceTo(2) // @code:advance
df.stepUntil(probe => probe.passed(1)) // @code:step
