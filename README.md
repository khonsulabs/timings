# timings

*This crate is early in development*.

A lightweight crate for timing and benchmarking.

This crate isn't ready for public consumption. The goal of the crate is to
easily create complex benchmarks like [BonsaiDb's Commerce
Benchmark][commerce-bench]. It supports collecting measurements from multiple
threads and/or async tasks.

## Timings vs Criterion

Benchmarking is [very tricky][nebari-benchmark-confusion], and it's important to
use the right tools for the right job. [Criterion][criterion] should be
preferred for most CPU-bound microbenchmarks, as it attempts to solve many tough
problems that can impact microbenchmark performance.

For operations that are short and CPU bound, Criterion will provide more
reliable results than Timings due to its use of sample-based measurement. By
executing the benchmarked function multiple times inbetween time measurements,
Criterion can be more confident that the performance of the benchmarked function
is what is being measured, not the overhead of collecting the timing
information.

For longer operations or operations that are IO bound, Timings will provide more
accurate results than Criterion. Because Criterion measures in samples, it
averages each batch of iterations in its reported statistics. This flattens
minimum and maximum measurements, which can be important when analyzing IO bound
performance. Timings on the other hand measures every invocation and reports the
statistics of all invocations.

[commerce-bench]: https://dev.bonsaidb.io/main/benchmarks/commerce/small-balanced/1/index.html
[criterion]: https://github.com/bheisler/criterion.rs
[nebari-benchmark-confusion]: https://bonsaidb.io/blog/durable-writes/#Why%20is%20Nebari%20slow%3F
