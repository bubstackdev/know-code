# review-v1's development set

`review` is tuned on this set, never on review-v1 itself, so the gate's answers are read once, for the gate.

[dev_set.py](dev_set.py) draws it from review-v1's own pool (`review_labels.pool`, at the same repositories and pins).
It leaves out every review that shares anything with the frozen set: its introducing commit, its fix, or, in the same
repository, any file either commit changed or labels. The draw is `review_labels.draw` with the seed `review-dev-v1`,
25 reviews per language where the repositories have them.

```sh
python3 eval/review-v1/dev/dev_set.py --work WORK --out DEV.jsonl --pools WORK/review-pools.json
```

The labels aren't committed. They are rebuilt from git with the command above, and [dev.sha256](dev.sha256) holds the
SHA-256 of the file it writes. The introducing commits and fixes tuning read are listed in [commits.txt](commits.txt),
so anyone can check that none is in [reviews.jsonl](../reviews.jsonl).

63 reviews: 25 C# (all jellyfin-server: autofac's and serilog's reviews all share a file with the frozen set), 25
TypeScript (13 hono, 12 jellyfin-web) and 13 Swift (12 swift-collections, 1 swift-nio), with 95 file labels and 16
test labels. Of each repository's pool, the reviews left after the disjointness rule were: swift-argument-parser 0 of 4,
swift-collections 12 of 33, swift-nio 1 of 12, serilog 0 of 3, autofac 0 of 8, jellyfin-server 111 of 225, jellyfin-web
82 of 202, hono 49 of 187.
