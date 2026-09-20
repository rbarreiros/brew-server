# Group priority / pre-emption

By default a higher numeric priority wins (`higher_priority_number_wins = true`). If TG 91 currently has priority 3 and another cell starts TG 91 at priority 7, the server:

1. Generates `GROUP_IDLE` for the displaced call UUID using `preempt_cause`.
2. Sends it to the old transmitting cell and all routed listening cells.
3. Removes the old call/floor state.
4. Installs and forwards the new higher-priority call.

Equal/lower priority attempts are rejected while the floor is occupied. Set `higher_priority_number_wins = false` if your deployed Brew/TETRA profile uses inverse priority ordering.
