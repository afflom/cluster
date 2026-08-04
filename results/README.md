# results

Measurement output, committed.

This is the only irreplaceable artifact the cluster produces (SPEC.md §5.6). It
is small, `lv_data` is a single copy with no replica and no off-site target, and
so the backup is git: committing it makes provenance a property of the same
history that produced the image the measurement was taken on.

Nothing prunes this directory. `model/policy.toml` records that explicitly
(`measurement_output_pruned = false`) so that the retention policy has to be
edited, deliberately, before anything here could be removed.

What a run may claim about what it produced is bounded by §21.1: the isolation
configured in §8.5 is constructible and `CB-` carries it, but the stability of
the environment is not, and a dispersion reported from here is an `open` claim
with its sample size and seed beside it --- measured and reported, never
asserted.
