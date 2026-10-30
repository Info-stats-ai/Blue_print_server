import unittest

from tasks.store import TaskStore


class TaskStoreTest(unittest.TestCase):
    def test_add_gives_increasing_ids(self):
        store = TaskStore()
        self.assertEqual(store.add("write docs").id, 1)
        self.assertEqual(store.add("ship it").id, 2)


if __name__ == "__main__":
    unittest.main()
