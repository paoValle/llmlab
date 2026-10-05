// The agent side of the vertical: agentloop, over the wire, through the gateway.
//
// This file is the honest half of the measurement. It is a real agentloop run with a real
// budget and a real trace: the gateway is not a mock, the agent is not a script, and the two
// account for the same money independently. If the two numbers disagree, one of the two
// implementations is wrong, and that is what the lab reports.
//
// It prints exactly one JSON line, which the Rust side parses: a process boundary is a
// contract, and a contract that is one line of JSON is a contract that can be tested.
//
//   tsx agent/run.ts --base-url http://127.0.0.1:PORT --api-key sk-acme --scenario happy

import { writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

import {
  Budget,
  Trace,
  micros,
  openAICompatible,
  replay,
  run,
  usd,
  type PriceTable,
} from '../../agentloop/src/index.js';

const { values } = parseArgs({
  options: {
    'base-url': { type: 'string' },
    'api-key': { type: 'string' },
    model: { type: 'string' },
    scenario: { type: 'string' },
    'trace-out': { type: 'string' },
  },
});

const baseUrl = values['base-url'];
const apiKey = values['api-key'];
const scenario = values.scenario ?? 'happy';
if (baseUrl === undefined || apiKey === undefined) {
  console.log(JSON.stringify({ scenario, ok: false, error: 'usage', message: '--base-url and --api-key are required' }));
  process.exit(2);
}

const model = values.model ?? 'gpt-4o-mini';

// The same price list the gateway is configured with, in micro-dollars per million tokens.
// The two sides doing the arithmetic independently is the point: same input, same number.
const PRICES: PriceTable = { [model]: { input: micros(150), output: micros(600) } };

const trace = new Trace({ clock: () => 1_700_000_000_000 });
const messages = [
  { role: 'user' as const, content: 'How much does the cheapest flight from Naples to Rome cost?' },
];

const policy = openAICompatible({
  model,
  apiKey,
  baseUrl,
  maxOutputTokens: 64,
  timeoutMs: 15_000,
});

try {
  const result = await run({
    policy,
    messages,
    budget: new Budget(usd(1)),
    prices: PRICES,
    maxSteps: 3,
    trace,
    runId: `e2e-${scenario}`,
  });

  // The agent's own reproducibility, checked offline from its own trace: no gateway, no
  // network, the same loop with a policy that reads from disk.
  const { equal } = await replay(trace);
  if (values['trace-out'] !== undefined) writeFileSync(values['trace-out'], trace.toJSONL());

  console.log(
    JSON.stringify({
      scenario,
      ok: true,
      steps: result.steps,
      stopReason: result.stopReason,
      answer: result.answer ?? null,
      spentMicroUsd: result.spent,
      replayEqual: equal,
    }),
  );
} catch (error) {
  // A hard refusal from the gateway (a 429 from the cap) is not a crash to hide: it is what
  // the agent's policy does with it, and the lab reports that verbatim. The whole cause chain
  // is included, because "the Policy failed at step 0" is the symptom, not the reason.
  const chain: string[] = [];
  let current: unknown = error;
  while (current instanceof Error) {
    chain.push(`${current.name}: ${current.message}`);
    current = (current as { cause?: unknown }).cause;
  }
  console.log(
    JSON.stringify({
      scenario,
      ok: false,
      steps: 0,
      stopReason: 'policy_error',
      answer: null,
      spentMicroUsd: 0,
      replayEqual: false,
      error: (error as Error).constructor.name,
      message: chain.join(' <- '),
    }),
  );
}
