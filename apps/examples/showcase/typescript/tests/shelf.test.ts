// Resources that make and take resources, and the JavaScript features that
// they implement.
import assert from "node:assert/strict";
import { test } from "node:test";
import { inspect } from "node:util";

import {
  AsyncSession,
  Labeler,
  Session,
  Shelf,
  ShelfError,
  Shortage,
  TitleStream,
  Titles,
  countBoth,
  doubleSize,
  offset,
} from "showcase/shelf";

function shelf(titles: string[] = [], limit = 3): Shelf {
  const result = new Shelf("fiction", limit);
  for (const title of titles) result.add(title);
  return result;
}

test("defaults", () => {
  assert.equal(new Shelf().name, "shelf");
  assert.equal(new Shelf("a").name, "a");
  assert.equal(offset(3), 1);
  assert.equal(offset(3, 1), 2);
  assert.equal(offset(3, undefined, 2), 4);
});

test("pointer-sized integers are bigints", () => {
  assert.equal(doubleSize(21n), 42n);
  assert.throws(() => doubleSize(-1n), TypeError);
});

test("getter and setter", () => {
  const value = shelf();
  assert.equal(value.name, "fiction");
  value.name = "poetry";
  assert.equal(value.name, "poetry");
});

test("length, display, and inspect", () => {
  assert.equal(shelf(["a", "b"]).length, 2);
  assert.equal(String(shelf()), "the fiction shelf");
  assert.equal(inspect(shelf(["a"])), 'Shelf("fiction", 1 books)');
});

test("protocols stay methods", () => {
  const value = shelf(["a", "b"]);
  assert.equal(value.has("a"), true);
  assert.equal(value.at(1), "b");
  assert.equal(value.at(9), null);
  value.replace(0, "c");
  value.remove(1);
  assert.equal(value.size(), 1n);
  assert.equal(value.occupied(), true);
  assert.deepEqual(value.matching("c"), ["c"]);
  assert.equal(value.same(shelf(["c"])), true);
  assert.equal(value.fingerprint(), 1);
});

test("optional bytes", () => {
  assert.deepEqual(shelf(["a"]).firstBytes(), new Uint8Array([97]));
  assert.equal(shelf().firstBytes(), null);
});

test("closed says whether it closed", () => {
  const value = shelf();
  assert.equal(value.closed, false);
  value.close();
  assert.equal(value.closed, true);
});

test("a method returns a resource", () => {
  assert.ok(shelf().session() instanceof Session);
});

test("a returned resource has no constructor", () => {
  assert.throws(() => new (Session as unknown as new () => Session)(), /other exports return it/);
});

test("an option and a list of resources", () => {
  assert.ok(shelf().trySession() instanceof Session);
  assert.equal(shelf(["a"], 1).trySession(), null);
  const parts = shelf(["a", "b", "c"]).split(2);
  assert.deepEqual(parts.map((part) => part.length), [2, 1]);
  assert.ok(parts.every((part) => part instanceof Shelf));
});

test("a shared borrow reads and leaves it open", () => {
  const target = shelf();
  const source = shelf(["a", "b"]);
  target.copyFrom(source);
  assert.equal(target.length, 2);
  assert.equal(source.length, 2);
});

test("an exclusive borrow", () => {
  const source = shelf(["a"]);
  const target = shelf(["b"]);
  source.giveTo(target);
  assert.equal(source.length, 0);
  assert.equal(target.length, 2);
});

test("taking a resource closes it", () => {
  const target = shelf(["a"]);
  const source = shelf(["b"]);
  target.absorb(source);
  assert.equal(target.length, 2);
  assert.equal(source.closed, true);
  assert.throws(() => target.absorb(source), /closed/);
});

test("an optional resource", () => {
  assert.equal(shelf(["a"]).countWith(null), 1);
  assert.equal(shelf(["a"]).countWith(), 1);
  assert.equal(shelf(["a"]).countWith(shelf(["b", "c"])), 3);
});

test("another class is a TypeError", () => {
  assert.throws(() => shelf().copyFrom("a" as unknown as Shelf), TypeError);
});

test("one resource cannot be passed twice", () => {
  const value = shelf(["a"]);
  assert.throws(() => value.copyFrom(value), /passed twice/);
  assert.throws(() => value.giveTo(value), /passed twice/);
  assert.throws(() => value.countWith(value), /passed twice/);
  assert.throws(() => countBoth(value, value), /passed twice/);
  assert.equal(countBoth(value, shelf(["b", "c"])), 3n);
});

test("a self method closes the resource", () => {
  const value = shelf(["a", "b"]);
  assert.deepEqual(value.seal(), ["a", "b"]);
  assert.equal(value.closed, true);
  assert.throws(() => value.seal(), /closed/);
});

test("using tells exit that the block failed, so it needs an explicit commit", () => {
  const value = shelf();
  let kept: Session;
  {
    using session = value.session();
    kept = session;
    session.add("a");
  }
  assert.equal(value.length, 0);
  assert.equal(kept.closed, true);
  {
    using session = value.session();
    session.add("a");
    session.commit();
  }
  assert.equal(value.length, 1);
});

test("using closes a resource without hooks", () => {
  let kept: Shelf;
  {
    using value = shelf();
    kept = value;
  }
  assert.equal(kept.closed, true);
});

test("a closed resource skips exit", () => {
  const value = shelf();
  {
    using session = value.session();
    session.add("a");
    session.close();
  }
  assert.equal(value.length, 0);
});

test("for of walks to the end", () => {
  const titles = shelf(["a", "b"]).titles();
  assert.ok(titles instanceof Titles);
  assert.deepEqual([...titles], ["a", "b"]);
  assert.deepEqual([...titles], []);
});

test("details are typed", () => {
  assert.throws(
    () => shelf(["a"], 1).add("b"),
    (error: unknown) => {
      assert.ok(error instanceof ShelfError);
      assert.equal(error.code, "full");
      assert.deepEqual(error.details, { limit: 1, title: "b" });
      return true;
    },
  );
});

test("a code without details has null", () => {
  assert.throws(
    () => shelf().add(""),
    (error: unknown) => error instanceof ShelfError && error.code === "untitled" && error.details === null,
  );
});

test("an error struct declares details too", () => {
  shelf(["a"]).require(1);
  assert.throws(
    () => shelf(["a"]).require(2),
    (error: unknown) => {
      assert.ok(error instanceof Shortage);
      assert.equal(error.code, "shortage");
      assert.deepEqual(error.details, { held: 1, wanted: 2 });
      return true;
    },
  );
});

test("a function runs and can stop", () => {
  const seen: Array<[string, number]> = [];
  const count = shelf(["a", "b", "c"]).visit((title, index) => {
    seen.push([title, index]);
    return index < 1;
  });
  assert.equal(count, 2);
  assert.deepEqual(seen, [["a", 0], ["b", 1]]);
});

test("a function can read the resource it was called from", () => {
  const value = shelf(["a"]);
  assert.equal(value.visit(() => value.length === 1), 1);
});

test("a function cannot change the resource it was called from", () => {
  const value = shelf(["a"]);
  assert.throws(
    () =>
      value.visit(() => {
        value.add("b");
        return true;
      }),
    (error: unknown) => error instanceof ShelfError && error.code === "visitor" && /Shelf\.add\(\): the resource is busy/.test(error.message),
  );
  assert.equal(value.length, 1);
});

test("a function cannot read a resource that its call holds alone", () => {
  const value = shelf(["a"]);
  assert.throws(
    () => value.rewrite((title) => `${title}:${value.length}`),
    (error: unknown) => error instanceof ShelfError && /the resource is busy/.test(error.message),
  );
});

test("a function can use another resource", () => {
  const value = shelf(["a"]);
  const other = shelf();
  value.rewrite((title) => {
    other.add(title);
    return title.toUpperCase();
  });
  assert.equal(value.at(0), "A");
  assert.equal(other.at(0), "a");
});

test("a function from an async call cannot change its own resource", async () => {
  const value = shelf();
  await assert.rejects(
    value.labelLater((name) => {
      value.add("b");
      return name;
    }),
    (error: unknown) => error instanceof ShelfError && /Shelf\.add\(\): the resource is busy/.test(error.message),
  );
});

test("an exception reaches the caller as text", () => {
  assert.throws(
    () =>
      shelf(["a"]).visit(() => {
        throw new Error("no such book");
      }),
    (error: unknown) => error instanceof ShelfError && /no such book/.test(error.message),
  );
});

test("a value that is not a function is a TypeError", () => {
  assert.throws(() => shelf().visit("no" as unknown as () => boolean), TypeError);
});

test("Rust keeps a function after the call", () => {
  const labeler = shelf().labeler((name) => name.toUpperCase());
  assert.ok(labeler instanceof Labeler);
  assert.equal(labeler.label("ada"), "ADA");
});

test("an async method returns a resource", async () => {
  assert.ok((await shelf().asyncSession()) instanceof AsyncSession);
});

test("await using runs async exit", async () => {
  const value = shelf();
  const session = await value.asyncSession();
  assert.equal(await session.open(), 0);
  assert.equal(session.opened, true);
  {
    await using held = session;
    held.add("a");
    await held.commit();
  }
  assert.equal(value.length, 1);
  assert.equal(session.closed, true);
});

test("a plain using refuses an async exit", async () => {
  const session = await shelf().asyncSession();
  const dispose = (session as unknown as Record<symbol, () => void>)[Symbol.dispose];
  assert.throws(() => dispose?.call(session), /needs `await using`/);
});

test("for await walks to the end", async () => {
  const stream = shelf(["a", "b"]).titleStream();
  assert.ok(stream instanceof TitleStream);
  const titles: string[] = [];
  for await (const title of stream) titles.push(title);
  assert.deepEqual(titles, ["a", "b"]);
});

test("an async call borrows a resource", async () => {
  const target = shelf();
  await target.copyLater(shelf(["a"]));
  assert.equal(target.length, 1);
});

test("an async self method closes the resource", async () => {
  const value = shelf(["a", "b"]);
  assert.equal(await value.sealLater(), 2n);
  assert.equal(value.closed, true);
});

test("a function runs from an async call", async () => {
  assert.equal(await shelf().labelLater((name) => `${name}!`), "fiction!");
});

test("a function from an async call can call sync methods", async () => {
  const other = shelf(["a", "b"]);
  assert.equal(await shelf().labelLater((name) => `${name}:${other.length}`), "fiction:2");
});

test("a function that throws something without a prototype", () => {
  assert.throws(
    () =>
      shelf(["a"]).visit(() => {
        throw Object.create(null);
      }),
    ShelfError,
  );
  assert.equal(shelf(["a"]).length, 1);
});
