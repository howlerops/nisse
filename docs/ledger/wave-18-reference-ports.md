# Wave 18 · Reference app ports

PRD phases 7–9. Real public TypeScript apps ported end to end. Each port is the gate for its phase, and its write-up feeds the rules table and the next waves. Candidates are in the PRD's [Reference apps](../PRD.md#reference-apps).

**Gate:** tier 3 shipped as one binary, passing its tests, with replay parity ≥ 99.9%.

- [x] **W18-01** Select the reference apps (D-15). Done by the [D-15 spike](../spikes/D-15-reference-apps.md); re-pin the commits when Phase 7 starts.
- [ ] **W18-02** Tier 1 port (gothinkster Express+Prisma RealWorld): `nisse port`, agent pass, verification.
  *Needs:* waves 14–15, W16-14, W16-15. *Done when:* the Jan 2024 RealWorld API collection passes as on Node (309/311), its own tests pass as on Node, replay parity ≥ 99.9%, port automation ≥ 90%.
- [ ] **W18-09** (optional) Tier 1b port (mutoe NestJS RealWorld), as an early check of decorators and NestJS.
  *Needs:* W18-02, W10-19. *Done when:* the same API collection passes.
- [ ] **W18-03** Tier 1 performance report vs the original on Node.
  *Needs:* W18-02, W15-10. *Done when:* published; feeds the PRD's ported-performance metric.
- [ ] **W18-04** Tier 2 backend port (Umami's API and data layer).
  *Needs:* W18-02, wave 16. *Done when:* its tests pass and replay parity ≥ 99.9%.
- [ ] **W18-05** Tier 2 frontend port, completing the app as one binary.
  *Needs:* W18-04, W17-09. *Done when:* the full app runs from one binary and its end-to-end tests pass.
- [ ] **W18-06** Tier 3 port (Immich server and web; Hoppscotch if Immich's media pipeline blocks it).
  *Needs:* W18-05. *Done when:* the app ships as one binary, its tests pass, replay parity ≥ 99.9%, port automation ≥ 80%.
- [ ] **W18-07** Port write-up per tier: what the tool did, what the agent did, what humans did, and the new rules and ledger items it produced.
  *Needs:* each port. *Done when:* committed under `docs/ports/` for every tier.
- [ ] **W18-08** Publish the ports as public examples and case studies.
  *Needs:* W18-07. *Done when:* each is a public repository linked from the docs.
