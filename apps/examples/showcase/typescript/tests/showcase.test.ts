// Call every export of the showcase contract through the Wasm module, and in
// the copy that `apps/examples/run` builds, through the Node.js addon. `tsc`
// also checks this file against the generated declarations.

import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readdirSync, rmSync, statSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe as suite, it } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  Float64,
  RecordBatch,
  RecordBatchReader,
  Schema,
  Table,
  Vector,
  tableFromArrays,
  tableToIPC,
  vectorFromArray,
} from "apache-arrow";

import {
  CONTRACT_VERSION,
  DEFAULT_LABEL,
  FIRST_STROKE,
  Ledger,
  NO_STROKES,
  PAUSE,
  LockedError,
  Mailbox,
  ParseError,
  Priority,
  bounds,
  corners,
  countTags,
  countWords,
  describe,
  echoNumbers,
  echoRecord,
  echoSettings,
  fingerprint,
  finish,
  longer,
  nativeReady,
  nextDay,
  observe,
  parsePriority,
  ping,
  qualify,
  retrace,
  reverse,
  routeLength,
  runningTotal,
  scale,
  total,
  totals,
  touch,
  wrap,
  type Event,
  type JsonValue,
  type Numbers,
  type ParseErrorCode,
  type Record as ShowcaseRecord,
  type Stroke,
} from "showcase";
import {
  ColumnError,
  type RecordBatchStream,
  addTotals,
  batches,
  countRows,
  describe as describeSchema,
  scale as scaleColumn,
  split,
  sumColumn,
  totals as streamTotals,
  withNote,
} from "showcase/columns";
import { GeometryError, direction, distance, type Point } from "showcase/geometry";
import { phrase } from "showcase/text";

import { addon } from "./backend.ts";

await nativeReady;

// The private runtime, whose declarations list only `nativeReady`.
const runtime: {
  native: Record<string, (...args: unknown[]) => unknown>;
  nativeError(error: unknown, ErrorType?: unknown): unknown;
} = await import("../showcase/runtime.js" as string);

/** Load a second copy of apache-arrow, as a linked package can. */
async function otherArrowCopy(): Promise<typeof import("apache-arrow")> {
  const installed = dirname(fileURLToPath(import.meta.resolve("apache-arrow")));
  const modules = join(mkdtempSync(join(tmpdir(), "showcase-arrow-")), "node_modules");
  cpSync(installed, join(modules, "apache-arrow"), {
    recursive: true,
    filter: (path) => statSync(path).isDirectory() || path.endsWith(".mjs") || path.endsWith(".json"),
  });
  // Share apache-arrow's dependencies with the installed copy.
  for (const name of readdirSync(dirname(installed))) {
    if (name !== "apache-arrow" && !name.startsWith(".")) {
      symlinkSync(join(dirname(installed), name), join(modules, name), "junction");
    }
  }
  try {
    return await import(pathToFileURL(join(modules, "apache-arrow", "Arrow.node.mjs")).href);
  } finally {
    rmSync(dirname(modules), { recursive: true, force: true });
  }
}

function record(changes: Partial<ShowcaseRecord> = {}): ShowcaseRecord {
  return {
    id: "r-1",
    label: "First",
    priority: Priority.High,
    score: 90,
    stars: 4,
    note: null,
    tags: ["a", "b"],
    counters: { views: 3 },
    ranges: { week: [1, 7] },
    createdAt: "2026-07-22T12:00:00Z",
    localTime: "2026-07-22T09:00:00-03:00",
    metadata: { nested: [1, "two", null], wide: 2n ** 63n },
    checksum: [0, 255],
    location: { x: 1.5, y: -2 },
    revision: 0,
    ...changes,
  };
}

function errorCode(error: ParseError): string {
  switch (error.code) {
    case "empty":
      return "nothing to parse";
    case "unknown_value":
      return "not a priority";
  }
}

suite("models", () => {
  it("keep every number width; 64-bit integers are bigints", () => {
    const numbers: Numbers = {
      flag: true,
      tiny: 255,
      small: -32768,
      medium: 4_294_967_295,
      signed: -2_147_483_648,
      large: 2n ** 64n - 1n,
      signedLarge: -(2n ** 63n),
      single: 0.5,
      double: Math.PI,
    };

    assert.deepEqual(echoNumbers(numbers), numbers);
  });

  it("round-trip every field type and come back frozen", () => {
    const echoed = echoRecord(record());

    assert.deepEqual(echoed, record());
    assert.ok(Object.isFrozen(echoed));
    assert.ok(Object.isFrozen(echoed.tags));
  });

  it("fill defaults that the caller leaves out", () => {
    assert.deepEqual(echoSettings({ version: "v1" }), { version: "v1", verbose: false, prefix: "" });
  });

  it("keep the field rules, which Rust checks as RangeErrors that name the field", () => {
    const brokenRule = (message: string) => (error: unknown) =>
      error instanceof RangeError && error.message === message;

    assert.throws(
      () => echoRecord(record({ label: "x".repeat(41) })),
      brokenRule("echoRecord(): argument `record`: `label` must have at most 40 characters, got 41"),
    );
    assert.throws(
      () => countTags([record(), record({ stars: 9 })]),
      brokenRule("countTags(): argument `records`: `[1].stars` must be at most 5, got 9"),
    );
    assert.throws(
      () => echoRecord(record({ tags: ["t", "t", "t", "t", "t", "t"] })),
      brokenRule("echoRecord(): argument `record`: `tags` must have at most 5 items, got 6"),
    );
    assert.throws(
      () => echoSettings({ version: "v2" } as never),
      brokenRule('echoSettings(): argument `settings`: `version` must be "v1", got "v2"'),
    );
    // Lengths count code points, as Python does, not UTF-16 units.
    const label = "\u{1F44D}".repeat(40);
    assert.equal(echoRecord(record({ label })).label, label);
  });

  it("pick a tagged variant by its tag", () => {
    const events: Event[] = [
      { kind: "created", recordId: "r-1" },
      { kind: "moved", recordId: "r-1", to: { x: 2, y: 3 } },
      { kind: "deleted", recordId: "r-1", reason: "duplicate" },
    ];

    assert.deepEqual(events.map(describe), ["created r-1", "moved r-1 to 2, 3", "deleted r-1: duplicate"]);
  });

  it("cross durations as nanoseconds and arrays as tuples", () => {
    assert.equal(longer(2_500_000_000n), 2_500_000_000n);
    assert.equal(longer(1n), 1_000_000_000n);
    assert.equal(total([1_500_000_000n, null, 2n]), 1_500_000_002n);
    assert.deepEqual(corners([2, 1]), [{ x: 0, y: 0 }, { x: 2, y: 0 }, { x: 2, y: 1 }, { x: 0, y: 1 }]);
    assert.deepEqual(NO_STROKES, []);
    assert.equal(PAUSE, 1_500_000_000n);
    assert.equal(nextDay("2028-02-28"), "2028-02-29");
    assert.equal(nextDay(), "2028-02-29");
    assert.deepEqual(observe({ date: "2027-12-25", observed: null }), {
      date: "2027-12-25",
      observed: "2027-12-27",
    });
    // Rust names the argument that holds a negative or fractional duration.
    assert.throws(() => longer(-1n), /first/);
    // @ts-expect-error A duration is a whole number of nanoseconds.
    assert.throws(() => longer(1.5), /first/);
  });

  it("cross an externally tagged variant as its name or under it", () => {
    const strokes: Stroke[] = ["lift", { move_to: { x: 1, y: 2 } }, { line_to: { to: { x: 3, y: 4 }, width: 0.5 } }];

    assert.deepEqual(retrace(strokes), [...strokes].reverse());
    assert.deepEqual(FIRST_STROKE, { move_to: { x: 0, y: 0 } });
    assert.deepEqual(retrace([{ again: { again: "lift" } }, { pause: null }]), [{ pause: null }, { again: { again: "lift" } }]);
    assert.deepEqual(finish([]), ["lift"]);
    // @ts-expect-error A stroke is one variant, which Rust also checks.
    const both: Stroke = { move_to: { x: 1, y: 2 }, line_to: { to: { x: 3, y: 4 }, width: 0.5 } };
    assert.throws(() => retrace([both]));
  });
});

suite("functions", () => {
  it("accept dates as ISO strings", () => {
    const touched = touch(record({ revision: 4 }), "2027-01-01T00:00:00Z");

    assert.equal(touched.revision, 5);
    assert.equal(touched.createdAt, "2027-01-01T00:00:00Z");
  });

  it("handle lists, maps, tuples, and options", () => {
    assert.deepEqual(countTags([record(), record({ tags: ["b"] })]), { a: 1, b: 2 });
    assert.deepEqual(bounds([3, -1, 7]), [-1, 7]);
    assert.equal(bounds([]), null);
    assert.deepEqual(totals([["a", 1], ["b", 2], ["a", 3]]), { a: 4, b: 2 });
  });

  it("keep a map key named __proto__ as data", () => {
    const counts = totals([["__proto__", 1], ["a", 2]]);
    assert.equal(Object.getPrototypeOf(counts), Object.prototype);
    assert.deepEqual(Object.entries(counts).sort(), [["__proto__", 1], ["a", 2]]);
    assert.ok(Object.isFrozen(counts));

    const { counters } = echoRecord(record({ counters: JSON.parse('{"__proto__": 3}') }));
    assert.ok(Object.hasOwn(counters, "__proto__"));
    assert.equal(counters["__proto__"], 3);

    const { wrapped } = wrap(JSON.parse('{"__proto__": {"nested": true}}')) as {
      readonly wrapped: { readonly [key: string]: JsonValue };
    };
    assert.equal(Object.getPrototypeOf(wrapped), Object.prototype);
    assert.deepEqual(Object.keys(wrapped), ["__proto__"]);
    assert.deepEqual(wrapped["__proto__"], { nested: true });
    assert.ok(Object.isFrozen(wrapped));
  });

  it("rename parameters that JavaScript reserves", () => {
    // `qualify(package, class)` would be a syntax error in a module.
    assert.equal(qualify("geometry", "Point"), "geometry.Point");
    assert.equal(qualify.length, 2);
  });

  it("keep JSON values, with wide integers as bigints", () => {
    const value = { list: [1, 2.5, "x", null, true], wide: 2n ** 64n - 1n };

    assert.deepEqual(wrap(value), { wrapped: value });
    // A number that is not a safe integer crosses as a float.
    assert.deepEqual(wrap([2 ** 53, 1e300]), { wrapped: [2 ** 53, 1e300] });
  });

  it("return undefined for unit", () => {
    assert.equal(ping(), undefined);
  });

  it("pass bytes as Uint8Array", () => {
    assert.deepEqual(reverse(new Uint8Array([1, 2, 3])), new Uint8Array([3, 2, 1]));
    assert.deepEqual(fingerprint(new Uint8Array([1, 2, 3, 4, 5])), new Uint8Array([4, 2, 3, 4]));
  });

  it("pass buffers as typed arrays", () => {
    assert.deepEqual(runningTotal(new BigInt64Array([1n, 2n, 3n])), new BigInt64Array([1n, 3n, 6n]));
    assert.deepEqual(scale(new Float32Array([1, -2]), 0.5), new Float32Array([0.5, -1]));
  });

  it("export constants", () => {
    assert.equal(CONTRACT_VERSION, 3);
    assert.equal(DEFAULT_LABEL, "untitled");
  });
});

suite("errors", () => {
  it("carry a typed code and the Rust message", () => {
    assert.throws(
      () => parsePriority("urgent"),
      (error: unknown) =>
        error instanceof ParseError && errorCode(error) === "not a priority" && error.message === "unknown value `urgent`",
    );
    assert.throws(() => parsePriority(" "), (error: unknown) => error instanceof ParseError && error.code === "empty");
    assert.equal(parsePriority("high"), Priority.High);
  });

  it("reject codes that the error cannot report", () => {
    const code: ParseErrorCode = "empty";
    // @ts-expect-error `missing` is not a ParseError code.
    const unknown: ParseErrorCode = "missing";
    assert.notEqual(code, unknown);
  });

  it("come from other namespaces", () => {
    const origin: Point = { x: 0, y: 0 };
    assert.throws(
      () => routeLength([origin, origin]),
      (error: unknown) => error instanceof GeometryError && error.code === "same_point",
    );
    assert.equal(routeLength([origin, { x: 3, y: 4 }]), 5);
  });

  it("throw untyped failures as Error objects with a stack", () => {
    const ledger = new Ledger("Ada");
    ledger.close();

    for (const [call, method] of [
      [() => ledger.balance(), "balance"],
      [() => ledger.deposit(1n), "deposit"],
    ] as const) {
      assert.throws(
        call,
        (error: unknown) =>
          error instanceof Error &&
          !(error instanceof LockedError) &&
          error.message === `Ledger.${method}(): the resource is closed` &&
          typeof error.stack === "string",
      );
    }
  });

  it("name the function and the parameter of an argument of the wrong type", async () => {
    const wrongType = (message: string) => (error: unknown) =>
      error instanceof TypeError && error.message === message;

    assert.throws(
      () => qualify(1 as never, "Point"),
      wrongType("qualify(): argument `package_` must be a string, got number"),
    );
    assert.throws(
      () => (parsePriority as (...args: unknown[]) => unknown)(),
      wrongType("parsePriority(): argument `text` must be a string, got undefined"),
    );
    assert.throws(
      () => routeLength(null as never),
      wrongType("routeLength(): argument `route` must be an array, got null"),
    );
    assert.throws(
      () => describe("created" as never),
      wrongType("describe(): argument `event` must be an object, got string"),
    );
    // Buffers and bytes must be the typed array that the parameter declares.
    assert.throws(
      () => scale("x" as never, 2),
      wrongType("scale(): argument `samples` must be a Float32Array, got string"),
    );
    assert.throws(
      () => scale(new Float64Array([1]) as never, 2),
      wrongType("scale(): argument `samples` must be a Float32Array, got Float64Array"),
    );
    assert.throws(
      () => reverse([1, 2] as never),
      wrongType("reverse(): argument `bytes` must be a Uint8Array, got array"),
    );
    assert.throws(
      () => new Ledger(1 as never),
      wrongType("new Ledger(): argument `owner` must be a string, got number"),
    );
    assert.throws(
      () => Ledger.withBalance("Ada", "100" as never),
      wrongType("Ledger.withBalance(): argument `balance` must be a bigint, got string"),
    );
    using ledger = new Ledger("Ada");
    assert.throws(
      () => ledger.deposit({} as never),
      wrongType("Ledger.deposit(): argument `amount` must be a bigint, got object"),
    );
    await assert.rejects(
      countWords(1 as never),
      wrongType("countWords(): argument `text` must be a string, got number"),
    );
    // A `Map` or `Set` would cross as an empty object.
    assert.throws(
      () => describe(new Map() as never),
      wrongType("describe(): argument `event` must be an object, got Map"),
    );
    assert.throws(
      () => echoRecord(record({ counters: new Map([["views", 3]]) as never })),
      wrongType("Map values cannot cross as objects; pass a plain object"),
    );
  });

  it("read only a model's own fields, whatever their names", () => {
    const event = { kind: "created", recordId: "r-1", constructor: 1, toString: 2 } as never;
    assert.equal(describe(event), "created r-1");
  });

  it("throw what Rust finds inside an argument as TypeErrors", () => {
    assert.throws(
      () => routeLength([{ x: "far", y: 0 }] as never),
      (error: unknown) =>
        error instanceof TypeError &&
        error.message === 'routeLength(): argument `route`: invalid type: string "far", expected f64',
    );
    assert.throws(
      () => describe({ kind: "renamed", recordId: "r-1" } as never),
      (error: unknown) =>
        error instanceof TypeError &&
        error.message.startsWith("describe(): argument `event`: unknown variant `renamed`"),
    );
    // A value of the wrong shape crosses as it is, rather than as an empty
    // list or map, or a list of characters.
    const inside = (label: string) => (error: unknown) =>
      error instanceof TypeError && error.message.startsWith(`${label}: `);
    for (const changes of [{ tags: 5 }, { tags: "ab" }, { counters: 7 }, { ranges: { week: 5 } }]) {
      assert.throws(() => echoRecord(record(changes as never)), inside("echoRecord(): argument `record`"));
    }
    assert.throws(() => totals([["a", 1], 5] as never), inside("totals(): argument `entries`"));
  });

  // The Wasm module would trap on these.
  it("throw a TypeError for a value nested too deep for the addon", { skip: !addon }, () => {
    const tooDeep = (label: string) => (error: unknown) =>
      error instanceof TypeError && error.message === `${label} must nest at most 128 levels deep`;
    let deep: JsonValue = 0;
    for (let level = 0; level < 1000; level += 1) deep = [deep];
    assert.throws(() => wrap(deep), tooDeep("wrap(): argument `value`"));

    // `prepareHost` copies every argument, which overflows the JavaScript
    // stack for a cyclic value, so it goes to the addon's export directly.
    const exported = (wrapper: (...args: never[]) => unknown) => {
      const name = /\$native\["(\w+)"\]/.exec(String(wrapper))?.[1];
      assert.ok(name !== undefined, "the wrapper calls one native export");
      return runtime.native[name];
    };
    const cyclic: unknown[] = [];
    cyclic.push(cyclic);
    assert.throws(() => exported(wrap)(cyclic), tooDeep("wrap(): argument `value`"));
    assert.throws(
      () => exported(routeLength)([{ x: cyclic, y: 0 }]),
      (error: unknown) =>
        error instanceof TypeError &&
        error.message === "routeLength(): argument `route`: invalid type: JsValue([[…]]), expected f64",
    );
  });

  it("never read an untyped message that spans lines as a typed error", () => {
    const untyped = new Error("empty\nthe text is empty");
    assert.equal(runtime.nativeError(untyped, ParseError), untyped);
  });
});

suite("namespaces", () => {
  it("expose linked packages and Rust modules as subpaths", () => {
    assert.equal(distance({ x: 0, y: 0 }, { x: 3, y: 4 }), 5);
    assert.deepEqual(direction({ x: 0, y: 0 }, { x: 0, y: 2 }), { x: 0, y: 1 });
    assert.deepEqual(phrase("one two  three"), { text: "one two  three", wordCount: 3 });
  });
});

suite("resources", () => {
  it("keep state in Rust and report errors", () => {
    using ledger = new Ledger("Ada");
    assert.equal(ledger.deposit(10n), 10n);
    assert.equal(ledger.deposit(-3n), 7n);
    ledger.lock();
    assert.throws(
      () => ledger.deposit(1n),
      (error: unknown) => error instanceof LockedError && error.code === "locked_error",
    );
    assert.equal(ledger.owner(), "Ada");
    assert.deepEqual(ledger.history(), [
      { amount: 10n, balance: 10n },
      { amount: -3n, balance: 7n },
    ]);
  });

  it("offer extra constructors and constructor errors", () => {
    using ledger = Ledger.withBalance("Grace", 100n);
    assert.equal(ledger.balance(), 100n);
    assert.throws(() => new Ledger(" "), (error: unknown) => error instanceof ParseError);
  });

  it("hide skipped methods", () => {
    assert.equal("clear" in Ledger.prototype, false);
  });

  it("fail after close", () => {
    const ledger = new Ledger("Ada");
    ledger.close();

    assert.throws(() => ledger.balance(), /resource is closed/);
  });
});

suite("async", () => {
  it("returns promises from async functions", async () => {
    assert.equal(await countWords("one two three"), 3);
    assert.deepEqual(await Promise.all([countWords("a"), countWords("a b")]), [1, 2]);
  });

  it("opens resources with async factories and awaits their methods", async () => {
    await using mailbox = await Mailbox.open("Ada");
    assert.equal(await mailbox.deliver("hello there"), 1);
    assert.equal(await mailbox.deliver("goodbye"), 2);
    assert.equal(mailbox.count(), 2);
    assert.deepEqual(await mailbox.search("hello"), ["hello there"]);
    assert.equal(mailbox.recipient(), "Ada");
  });

  it("rejects with typed errors and has no constructor", async () => {
    await assert.rejects(
      Mailbox.open(" "),
      (error: unknown) => error instanceof ParseError && error.code === "empty",
    );
    await assert.rejects(
      Mailbox.open(42 as never),
      (error: unknown) => error instanceof TypeError && !(error instanceof ParseError),
    );
    assert.throws(() => Reflect.construct(Mailbox, []), TypeError);
  });

  it("reports a sync call that a pending async call blocks", async () => {
    await using mailbox = await Mailbox.open("Ada");
    // The delivery holds the mailbox from the call until it finishes.
    const delivery = mailbox.deliver("hello");
    assert.throws(
      () => mailbox.count(),
      (error: unknown) =>
        error instanceof Error &&
        error.message === "Mailbox.count(): the resource is busy with another call",
    );
    assert.equal(await delivery, 1);
    assert.equal(mailbox.count(), 1);
  });

  it("finishes a call that overlaps close", async () => {
    const mailbox = await Mailbox.open("Ada");
    const delivery = mailbox.deliver("late");
    mailbox.close();

    assert.equal(await delivery, 1);
    assert.throws(() => mailbox.count(), /resource is closed/);
  });
});

suite("arrow", () => {
  const prices = () =>
    tableFromArrays({
      price: new Float64Array([2, 3.5]),
      quantity: new BigInt64Array([3n, 2n]),
    });

  it("passes vectors and keeps nulls", () => {
    const scaled = scaleColumn(vectorFromArray([1, null, 2.5], new Float64()), 2);

    assert.ok(scaled instanceof Vector);
    assert.deepEqual(scaled.toJSON(), [2, null, 5]);
    assert.throws(
      () => scaleColumn(vectorFromArray([1n, 2n]), 2),
      (error: unknown) => error instanceof ColumnError && error.code === "wrong_type",
    );
  });

  it("joins a chunked vector into one array", () => {
    const table = split(prices().batches[0]!, 1);
    const column = table.getChild("price")!;

    assert.equal(column.data.length, 2);
    assert.deepEqual(scaleColumn(column, 2).toJSON(), [4, 7]);
  });

  it("passes record batches", () => {
    const totals = addTotals(prices().batches[0]!);

    assert.ok(totals instanceof RecordBatch);
    assert.deepEqual(
      totals.schema.fields.map((field) => field.name),
      ["price", "quantity", "total"],
    );
    assert.deepEqual(totals.getChild("total")?.toJSON(), [6, 7]);
  });

  it("returns tables with their batches", () => {
    const table = split(tableFromArrays({ n: new Int32Array([0, 1, 2, 3, 4]) }).batches[0]!, 2);

    assert.ok(table instanceof Table);
    assert.equal(table.numRows, 5);
    assert.deepEqual(table.batches.map((batch) => batch.numRows), [2, 2, 1]);
  });

  it("passes schemas with their metadata", () => {
    const schema = prices().schema;

    assert.deepEqual(describeSchema(schema), ["price: Float64", "quantity: Int64"]);
    const noted = withNote(schema, "prices in euros");
    assert.ok(noted instanceof Schema);
    assert.equal(noted.metadata.get("note"), "prices in euros");
  });

  it("awaits async calls that take Arrow values", async () => {
    assert.equal(await countRows(prices()), 2n);
  });

  it("explains values from another copy of apache-arrow", async () => {
    const other = await otherArrowCopy();
    const table = other.tableFromArrays({
      price: new Float64Array([2, 3.5]),
      quantity: new BigInt64Array([3n, 2n]),
    });
    const isCopyError = (error: unknown) =>
      error instanceof TypeError && /comes from a different copy of apache-arrow/.test(error.message);

    // The copies recognize each other's values, but each encodes only its own.
    assert.ok(table instanceof Table);
    assert.throws(() => scaleColumn(table.getChild("price")!, 2), isCopyError);
    assert.throws(() => addTotals(table.batches[0]!), isCopyError);
    assert.throws(() => describeSchema(table.schema), isCopyError);
    await assert.rejects(countRows(table), isCopyError);
  });

  it("rejects values that are not Arrow values", () => {
    assert.throws(
      () => scaleColumn([1, 2] as never, 2),
      (error: unknown) => error instanceof TypeError && error.message === "Expected an apache-arrow Vector",
    );
  });
});

suite("arrow streams", () => {
  const prices = (rows: number) =>
    tableFromArrays({
      price: Float64Array.from({ length: rows }, (_, row) => row + 0.5),
      quantity: BigInt64Array.from({ length: rows }, () => 2n),
    });

  /**
   * Batches that a generator makes as Rust reads them, with their schema, so
   * that nothing reads one early to find it, and whether the generator closed.
   */
  function source(count: number, failAt?: number) {
    const state = { made: 0, closed: false };
    async function* generate() {
      try {
        for (let index = 0; index < count; index++) {
          if (index === failAt) throw new Error("the source broke");
          state.made++;
          yield prices(2).batches[0]!;
        }
      } finally {
        state.closed = true;
      }
    }
    return { state, batches: Object.assign(generate(), { schema: prices(2).schema }) };
  }

  /** Wait until `done()`, which the JavaScript thread settles later. */
  async function eventually(done: () => boolean): Promise<void> {
    for (let tries = 0; tries < 200 && !done(); tries++) {
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    assert.ok(done());
  }

  it("returns a stream that reads one batch at a time", async () => {
    const stream: RecordBatchStream = batches(split(prices(5).batches[0]!, 2));

    assert.deepEqual(
      stream.schema.fields.map((field) => field.name),
      ["price", "quantity"],
    );
    const read: number[] = [];
    for await (const batch of stream) {
      assert.ok(batch instanceof RecordBatch);
      read.push(batch.numRows);
    }
    assert.deepEqual(read, [2, 2, 1]);
    assert.deepEqual(await stream.next(), { done: true, value: undefined });
  });

  it("reads tables, readers, streams and iterables of batches", async () => {
    const table = prices(4);
    const total = 0.5 + 1.5 + 2.5 + 3.5;

    assert.equal(await sumColumn(table, "price"), total);
    assert.equal(await sumColumn(RecordBatchReader.from(tableToIPC(table)), "price"), total);
    assert.equal(await sumColumn(batches(table), "price"), total);
    // An iterable without a schema gives its first batch's.
    assert.equal(await sumColumn(table.batches, "price"), total);
    // A table's batches lack its schema's metadata, which the stream keeps.
    const noted = new Table(new Schema(table.schema.fields, new Map([["note", "euros"]])), table.batches);
    assert.equal(await sumColumn(noted, "price"), total);
    assert.equal(await sumColumn(source(3).batches, "price"), 3 * (0.5 + 1.5));
  });

  it("passes a stream through Rust as the reader asks", async () => {
    const { state, batches: written } = source(5);
    const stream = await streamTotals(written);

    assert.equal(state.made, 0);
    const first = await stream.next();
    assert.deepEqual(first.value?.getChild("total")?.toJSON(), [1, 3]);
    assert.equal(state.made, 1);
  });

  it("closes a source once the reader stops", async () => {
    const { state, batches: written } = source(5);
    for await (const batch of await streamTotals(written)) {
      assert.equal(batch.numRows, 2);
      break;
    }
    await eventually(() => state.closed);
    assert.ok(state.made < 5);
  });

  it("fails every later read after an error", async () => {
    const stream = await streamTotals(source(3, 1).batches);

    assert.equal((await stream.next()).value?.numRows, 2);
    for (let read = 0; read < 2; read++) {
      await assert.rejects(stream.next(), /^Error: read: External error: the source broke$/);
    }
  });

  it("keeps a typed error's code in the message", async () => {
    const stream = await streamTotals([tableFromArrays({ n: new Float64Array([1]) }).batches[0]!]);

    await assert.rejects(stream.next(), {
      message: "missing_column: the batch has no such column",
    });
  });

  it("needs a schema for an empty source", async () => {
    await assert.rejects(sumColumn([], "price"), {
      name: "TypeError",
      message: "An empty source has no schema: pass a Table, or an apache-arrow RecordBatchReader",
    });
  });
});
