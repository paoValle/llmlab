# llmlab — measured behaviour of the gateway

Generated 1791196013 (unix) · host: macos aarch64 · rustc 1.99.0 (b940084d7 2026-09-28) · llmgateway pinned by Cargo.lock.

Every number below comes from running the real gateway code in this process, with a fake
provider and no network. `make lab` regenerates this file; `cargo test` asserts it again.

## 1. The cap holds in front of the spend

10 requests, a cap of 0.000007 USD, one healthy provider, usage 1000 in / 500 out per request.

| measure | value |
|---|---|
| requests sent by the client | 10 |
| served with 200 | 5 |
| rejected with 429 by the cap | 5 |
| times the provider was called | 5 |
| money spent | 0.000005 USD |
| cap | 0.000007 USD |
| reserved and not settled at the end | 0.000000 USD |
| real cost of one request | 0.000001 USD |
| reserved estimate minus real cost | 0.000002 USD |

The provider was called exactly 5 times: a rejected request never reaches
it, which is the difference between a cap and a report. The reservation is pessimistic by
0.000002 USD per request — the estimate assumes the maximum output, the real response used
less. That surplus is why the cap rejects earlier than pure spending would: checking
before the call costs a little accuracy and buys the guarantee.

## 2. Failover: a provider error moves to the next provider

| measure | value |
|---|---|
| status the client saw | 200 |
| provider that served it | provider-b |
| provider calls | provider-a: 1, provider-b: 1 |
| money spent | 0.000001 USD |

## 3. Client error: it does not move anywhere

A 400 from the first provider is the client's fault, so no second provider is tried.

| measure | value |
|---|---|
| status the client saw | 400 |
| provider calls | provider-a: 1, provider-b: 0 |
| money spent | 0.000000 USD |

## 4. A request that may already have been executed is not retried

The first provider took the request and never answered: it may have executed it. Retrying
could charge twice, so the gateway stops and reports a failure.

| measure | value |
|---|---|
| status the client saw | 502 |
| provider calls | provider-a: 1, provider-b: 0 |
| money spent | 0.000000 USD |
| money still reserved | 0.000000 USD |

## 5. Money is exact, operating metrics are sampled

100 requests, sampling rate 1 in 1000.

| measure | value |
|---|---|
| money spent | 0.000100 USD |
| money expected (100 × per request) | 0.000100 USD |
| `served` as the meter reports it | 0 |
| requests served without the cap | 0 |

The two rows in the middle are the point: the money is exact to the micro-dollar, while
the operating counter reports 0 because it only counts 1 event in
1000. That is ADR 0005 made visible — invoice on the money, dashboard on the
counter, and never the other way round.

## 6. Streaming is forwarded, not accumulated

| measure | value |
|---|---|
| status | 200 |
| forwarded as a stream | true |
| money spent here | 0.000000 USD |

Zero, and it is declared rather than surprising: the usage of a streamed response arrives
in its last chunk, which the gateway never buffers, so the cost of a stream is metered
downstream by the client.

## 7. The agent, over the wire, through the gateway

agentloop (TypeScript, real budget, real trace) → HTTP → this gateway → HTTP → provider.

| measure | value |
|---|---|
| what the gateway metered | 0.000001 USD (1 µUSD) |
| what the agent's trace says it spent | 0.000001 USD (1 µUSD) |
| difference | 0 µUSD |
| steps / stop reason | 1 / end_turn |
| requests the provider served | 1 |
| the agent's own trace replays identically | true |
| final answer | The cheapest is Wizz at 41 euros. |

Two implementations of the same arithmetic — TypeScript in the runtime, Rust in the gateway — compute the cost of the same run from the same `usage`. A non-zero difference means one of them is wrong: reconciling a runtime's budget with a gateway's invoice is the whole reason both exist.

The same run for a tenant whose budget is already exhausted: the gateway answered **429**, the provider was not called, and the agent's policy raised `PolicyError` with

> PolicyError: the Policy failed at step 0 <- PolicyError: the provider responded 429 Too Many Requests: {"error":{"message":"monthly budget exhausted","type":"rate_limit_error"}}

A cap the runtime cannot talk its way around is a cap; a 429 that the agent retried as if it were a provider hiccup would not be.


