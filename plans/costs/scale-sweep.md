# scale sweep, 04d22a3, 2026-09-20

Command: `cargo test -q --release --test 14_scale -- --nocapture`. In-memory SQLite, a/b/c with n rows each, keys spread over n/10 groups. write ms = one single-row insert or delete on `a` with the view installed, mean of 40. recompute ms = the plain query, mean of 20 (3 at 10000, 1 at 100000). ratio = recompute / write.

```
query            n  populate ms     write ms recompute ms   ratio
chain           10         2.55        0.834        0.229    0.28
chain          100        12.52        4.944       15.020    3.04
chain         1000       151.55        8.977      234.878   26.16
chain        10000      1699.24       47.980     4046.800   84.34
chain       100000     17957.46      132.773    26277.643  197.91
group           10         0.63        0.018        0.004    0.24
group          100         0.51        0.020        0.021    1.07
group         1000         0.72        0.017        0.298   17.13
group        10000         2.46        0.019        1.536   81.34
group       100000        26.22        0.018       29.095 1590.87
distinct        10         0.91        0.055        0.002    0.04
distinct       100         0.85        0.053        0.006    0.12
distinct      1000         1.72        0.069        0.089    1.29
distinct     10000         8.74        0.056        0.241    4.31
distinct    100000       123.51        0.916        6.786    7.41
topk            10         2.92        0.083        0.015    0.18
topk           100         0.96        0.104        0.014    0.13
topk          1000         1.69        0.127        0.009    0.07
topk         10000         9.14        0.243        0.039    0.16
topk        100000        98.65        1.466        0.043    0.03
semijoin        10         1.30        0.092        0.010    0.10
semijoin       100         1.53        0.116        0.193    1.66
semijoin      1000         6.00        0.197        2.849   14.48
semijoin     10000        43.96        1.058        5.289    5.00
semijoin    100000       651.39        9.703      296.253   30.53
```
