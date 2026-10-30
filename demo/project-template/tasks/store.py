from dataclasses import dataclass, field


@dataclass
class Task:
    id: int
    title: str
    done: bool = False


@dataclass
class TaskStore:
    tasks: list[Task] = field(default_factory=list)

    def add(self, title: str) -> Task:
        task = Task(id=len(self.tasks) + 1, title=title)
        self.tasks.append(task)
        return task

    def complete(self, task_id: int) -> Task:
        task = self.tasks[task_id]
        task.done = True
        return task

    def pending(self) -> list[Task]:
        return [task for task in self.tasks if not task.done]
