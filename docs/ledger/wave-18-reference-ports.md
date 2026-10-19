# Wave 18 · Reference app ports

PRD phases 7–9. Real public TypeScript apps ported end to end. Each port is the gate for its phase, and its write-up feeds the rules table and the next waves. Candidates are in the PRD's [Reference apps](../PRD.md#reference-apps).

**Gate:** tier 3 shipped as one binary, passing its tests, with replay parity ≥ 99.9%.

- [ ] **W18-01** Select the reference apps (D-15): check license, size, test suite, activity and dependency tree for each candidate; pick one per tier and pin a commit.
  *Needs:* W13-01. *Done when:* D-15 is decided with the pinned commits recorded.
- [ ] **W18-02** Tier 1 port (a RealWorld TypeScript backend): `nisse port`, agent pass, verification.
  *Needs:* waves 14–15. *Done when:* its API test collection passes, its own tests pass, replay parity ≥ 99.9%, port automation ≥ 90%.
- [ ] **W18-03** Tier 1 performance report vs the original on Node.
  *Needs:* W18-02, W15-10. *Done when:* published; feeds the PRD's ported-performance metric.
- [ ] **W18-04** Tier 2 backend port (for example Umami's API and data layer).
  *Needs:* W18-02, wave 16. *Done when:* its tests pass and replay parity ≥ 99.9%.
- [ ] **W18-05** Tier 2 frontend port, completing the app as one binary.
  *Needs:* W18-04, W17-09. *Done when:* the full app runs from one binary and its end-to-end tests pass.
- [ ] **W18-06** Tier 3 port (for example Immich's server or Outline).
  *Needs:* W18-05. *Done when:* the app ships as one binary, its tests pass, replay parity ≥ 99.9%, port automation ≥ 80%.
- [ ] **W18-07** Port write-up per tier: what the tool did, what the agent did, what humans did, and the new rules and ledger items it produced.
  *Needs:* each port. *Done when:* committed under `docs/ports/` for every tier.
- [ ] **W18-08** Publish the ports as public examples and case studies.
  *Needs:* W18-07. *Done when:* each is a public repository linked from the docs.
