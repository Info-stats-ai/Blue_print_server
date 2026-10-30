import sys

from tasks.store import TaskStore


def main(argv: list[str]) -> None:
    store = TaskStore()
    for title in argv:
        store.add(title)
    if store.tasks:
        store.complete(1)
    for task in store.tasks:
        mark = "x" if task.done else " "
        print(f"[{mark}] {task.id}. {task.title}")


if __name__ == "__main__":
    main(sys.argv[1:])
