"""Bounded same-instance and mutable-input consumers of the installed wheel."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import threading

import rusty_haystack as rh


def changing_input(source, mutate, convert, check):
    ready = threading.Barrier(2)
    stop = threading.Event()
    started = threading.Event()
    halfway = threading.Event()
    changed_halfway = threading.Event()
    changes = []
    def writer():
        ready.wait(timeout=5)
        count = 0
        while not stop.is_set():
            mutate()
            count += 1
            started.set()
            if halfway.is_set():
                changed_halfway.set()
        changes.append(count)
    thread = threading.Thread(target=writer, daemon=True)
    thread.start()
    ready.wait(timeout=5)
    try:
        assert started.wait(5)
        for index in range(100):
            check(convert(source))
            if index == 49:
                halfway.set()
                assert changed_halfway.wait(5)
    finally:
        stop.set()
        thread.join(5)
    assert not thread.is_alive() and changes[0] > 0
    return {"conversions": 100, "mutations": changes[0]}


source = {"v" + str(i): rh.Int(i) for i in range(20000)}
def mutate_dict():
    source["changing"] = rh.Int(1)
    source.pop("changing", None)
def check_dict(value):
    assert len(value) in (20000, 20001)
    assert value["v123"].value == 123 and value["v19999"].value == 19999
dict_constructor = changing_input(source, mutate_dict, rh.HDict, check_dict)
dict_factory = changing_input(source, mutate_dict, rh.HDict.from_dict, check_dict)

items = [rh.Int(i) for i in range(2000)]
def mutate_list():
    items.append(rh.Int(2000))
    items.pop()
def check_list(value):
    assert len(value) in (2000, 2001)
    assert value[0].value == 0 and value[1999].value == 1999
source_list = changing_input(items, mutate_list, rh.HList, check_list)

shared_dict, shared_list, shared_grid = rh.HDict(), rh.HList(), rh.HGrid()
barrier = threading.Barrier(4)
def mutate_shared(worker):
    success = {"dict": 0, "list": 0, "grid": 0}
    rejected = 0
    barrier.wait(timeout=5)
    for index in range(100):
        value = rh.Int(worker * 100 + index)
        for name, call in (("dict", lambda: shared_dict.set(f"k{worker}_{index}", value)),
                           ("list", lambda: shared_list.push(value)),
                           ("grid", lambda: shared_grid.add_row(rh.HDict({"v": value})))):
            try:
                call()
                success[name] += 1
            except RuntimeError as error:
                # PyO3 mutable borrows reject overlap; they are not queued locks.
                assert "borrow" in str(error).lower(), error
                rejected += 1
        for read in (shared_dict.to_dict, lambda: list(shared_list), shared_grid.rows):
            try:
                snapshot = read()
                assert len(snapshot) <= 400
            except RuntimeError as error:
                assert "borrow" in str(error).lower(), error
                rejected += 1
    return {"success": success, "borrow_rejections": rejected}
with ThreadPoolExecutor(max_workers=4) as pool:
    outcomes = [future.result(timeout=20) for future in [pool.submit(mutate_shared, i) for i in range(4)]]
for name, value in (("dict", shared_dict), ("list", shared_list), ("grid", shared_grid)):
    count = sum(outcome["success"][name] for outcome in outcomes)
    assert count > 0 and len(value) == count, (name, count, len(value))
assert all(isinstance(value, rh.Int) for value in shared_dict.to_dict().values())
assert all(isinstance(value, rh.Int) for value in shared_list)
assert all(isinstance(row["v"], rh.Int) for row in shared_grid)

graph = rh.SharedGraph()
graph.add(rh.HDict({"id": rh.Ref("shared"), "left": rh.Int(0), "right": rh.Int(0)}))
barrier = threading.Barrier(4)
def graph_worker(worker):
    barrier.wait(timeout=5)
    for index in range(100):
        ref = f"w{worker}-{index}"
        graph.add(rh.HDict({"id": rh.Ref(ref), "point": rh.Marker()}))
        value = rh.Int(worker * 100 + index)
        graph.update("shared", rh.HDict({"left": value, "right": value}))
        snapshot = graph.get("shared")
        assert snapshot["left"].value == snapshot["right"].value
        snapshot.set("localOnly", rh.Marker())
        assert not graph.get("shared").has("localOnly")
        if index % 2 == 0:
            assert graph.remove(ref)["id"].val == ref
    return 100
with ThreadPoolExecutor(max_workers=4) as pool:
    counts = [future.result(timeout=20) for future in [pool.submit(graph_worker, i) for i in range(4)]]
assert sum(counts) == 400 and len(graph.all()) == 201 and len(graph.read("point")) == 200


async def lifecycles():
    for _ in range(12):
        app = rh.ApplicationBuilder(graph).start()
        await app.ready()
        pending = [app.read("point", limit=10) for _ in range(4)]
        closing = app.close()
        results = await asyncio.gather(*pending, return_exceptions=True)
        assert all(isinstance(value, (rh.HGrid, rh.ApplicationError)) for value in results), results
        await closing
        assert app.closed and (await app.terminated()).close_error is None
        try:
            app.read("point")
        except rh.ApplicationError:
            pass
        else:
            raise AssertionError("closed application admitted work")
    assert len(graph.read("point")) == 200


async def bounded():
    async with asyncio.timeout(30):
        await lifecycles()

asyncio.run(bounded())
print(json.dumps({"installed_package": str(Path(rh.__file__).resolve()), "source_dict_constructor": dict_constructor,
                  "source_dict_factory": dict_factory, "source_list": source_list, "shared_containers": outcomes,
                  "shared_graph_final_count": 201, "atomic_tag_snapshots": True, "returned_copy_independent": True,
                  "create_destroy_cycles": 12, "close_during_reads": True}, sort_keys=True))
