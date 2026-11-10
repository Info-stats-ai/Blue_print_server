import sys

from .api import serve
from .store import Store


def seeded_store() -> Store:
    store = Store()
    store.add_book("The Pragmatic Programmer", "Hunt and Thomas", 1999)
    store.add_book("Designing Data-Intensive Applications", "Kleppmann", 2017)
    store.add_book("Structure and Interpretation of Computer Programs", "Abelson and Sussman", 1985)
    store.add_member("Ada", "ada@example.test")
    store.add_member("Linus", "linus@example.test")
    return store


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["serve"]:
        serve(seeded_store())
        return 0
    store = seeded_store()
    for book in store.books.values():
        print(f"{book.id}: {book.title} ({book.author}, {book.year})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
