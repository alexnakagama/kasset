"""Integration check for add-play-queue, task 6.1.

Drives the release build of kasset in a pty (answering DSR), renders its
output with pyte, and asserts each step: an album track plays, tracks are
queued with `e` (three) and `E` (one), the queue view is reordered and
edited, queued tracks play in queue order (by `n` and by a track ending)
with the sidebar marking "Queue", kasset quits while a queued track plays,
and on restart that track is resumed paused, the rest of the queue plays,
then the album continues. Built on the harness of add-artist-album-browsing
(archive/2026-11-20-add-artist-album-browsing/e2e/e2e_browse.py). Usage:

    python3 -m venv venv && venv/bin/pip install pyte
    cargo build --release
    venv/bin/python e2e_queue.py target/release/kasset

Needs Linux (pty, ALSA output). Builds its library and state folder in a
temporary folder (kept, and its path printed, if a check fails), and writes
the recorded screens and the state file to e2e_queue_screens.txt next to
this script.
"""

import fcntl
import math
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import wave

import pyte

ROWS, COLS = 40, 120
RESULTS = []
SCREENS = []
RUNNING = []  # every Kasset started, killed on the way out if still alive


def check(name, ok, detail=""):
    RESULTS.append((name, ok))
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + (f"  [{detail}]" if detail else ""))


def tone(path, freq, seconds, rate=22050):
    frames = bytearray()
    for i in range(int(rate * seconds)):
        frames += struct.pack("<h", int(8000 * math.sin(2 * math.pi * freq * i / rate)))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(bytes(frames))


# (artist, album, [(title, seconds)]); Six is short so it ends by itself.
LIBRARY = [
    ("Alpha", "First", [("One", 60), ("Two", 60), ("Three", 60)]),
    ("Beta", "Gamma", [("Four", 60), ("Five", 60), ("Six", 4), ("Seven", 60)]),
]


def make_library(root):
    freq = 220
    for artist, album, titles in LIBRARY:
        folder = os.path.join(root, artist, album)
        os.makedirs(folder, exist_ok=True)
        for n, (title, secs) in enumerate(titles, 1):
            tone(os.path.join(folder, f"0{n} - {title}.wav"), freq, secs)
            freq += 55


class Kasset:
    def __init__(self, binary, library, state_home):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.lock = threading.Lock()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        env = dict(os.environ, XDG_STATE_HOME=state_home, TERM="xterm-256color")
        self.proc = subprocess.Popen(
            [binary, library], stdin=slave, stdout=slave, stderr=subprocess.PIPE,
            env=env, start_new_session=True,
        )
        os.close(slave)
        self.master = master
        self.alive = True
        RUNNING.append(self)
        self.reader = threading.Thread(target=self._pump, daemon=True)
        self.reader.start()
        self.wait_for(lambda s: "vol " in s, 10, "first frame")
        time.sleep(0.5)  # the first key right after startup can be lost

    def _pump(self):
        while self.alive:
            try:
                r, _, _ = select.select([self.master], [], [], 0.1)
                if not r:
                    continue
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            if b"\x1b[6n" in data:
                os.write(self.master, b"\x1b[1;1R")
            with self.lock:
                self.stream.feed(data)

    def text(self):
        with self.lock:
            return "\n".join(self.screen.display)

    def record(self, label):
        SCREENS.append((label, self.text()))

    def wait_for(self, pred, timeout, what):
        end = time.time() + timeout
        while time.time() < end:
            if pred(self.text()):
                return True
            time.sleep(0.05)
        raise RuntimeError(f"timeout waiting for {what}:\n{self.text()}")

    def send(self, keys, pause=0.2):
        for k in keys:
            os.write(self.master, k.encode())
            time.sleep(pause)

    def track_line(self):
        for line in self.text().splitlines():
            if "vol " in line:
                return line
        return ""

    def now(self):
        m = re.search(r"([▶‖■]) (\w+) — (\w+) · (\w+)", self.track_line())
        return m.groups() if m else None

    def playing(self, title):
        n = self.now()
        return n is not None and n[1] == title

    def status(self):
        """The status line: the line above the key bar."""
        return self.text().splitlines()[-2].strip()

    def main_title(self):
        # The main pane's top border, right of the sidebar.
        boxes = re.findall(r"┌ ([^┐]*?) ─*┐", self.text().splitlines()[0])
        return boxes[1].strip() if len(boxes) > 1 else ""

    def finish(self, timeout=10):
        if not self.alive:
            return self.proc.returncode, ""
        try:
            code = self.proc.wait(timeout)
        finally:
            self.alive = False
            self.reader.join()
            os.close(self.master)
        err = self.proc.stderr.read().decode(errors="replace")
        lines = err.splitlines(keepends=True)
        if lines and lines[0].startswith("Scanning ") and lines[0].rstrip().endswith("…"):
            lines = lines[1:]
        return code, "".join(lines)

    def quit(self):
        self.send("q")
        return self.finish()


def body_lines(text):
    """The lines of the pane row (between the top and bottom borders)."""
    out = []
    for line in text.splitlines()[1:]:
        if line.startswith("└"):
            break
        out.append(line)
    return out


def main_rows(text):
    """The main pane's track rows: (marker, title) in screen order, header
    excluded."""
    rows = []
    for line in body_lines(text):
        parts = line.split("│")
        if len(parts) > 3:
            m = re.match(r"^ ([▶‖■ ]) (\w+)\b", parts[3])
            if m and m.group(2) != "Title":
                rows.append((m.group(1), m.group(2)))
    return rows


def titles(text):
    return [t for _, t in main_rows(text)]


def marker(text, title):
    return next((m for m, t in main_rows(text) if t == title), None)


def sidebar_row(text, name):
    """The sidebar's part of the line whose sidebar row is `name`."""
    for line in text.splitlines():
        parts = line.split("│")
        if len(parts) > 2 and re.match(rf"^ [▶‖■ ] {name}\b", parts[1]):
            return parts[1]
    return ""


def sidebar_marked(text, name):
    row = sidebar_row(text, name)
    return len(row) > 1 and row[1] in "▶‖"


def queue_count(text):
    m = re.match(r"^ [▶‖■ ] Queue\s+(\d+)\s*$", sidebar_row(text, "Queue"))
    return int(m.group(1)) if m else None


def selected(k, pane):
    """Text of the focused selection (reverse video) in pane 0 (sidebar) or
    1 (main), or None."""
    with k.lock:
        for y in range(1, ROWS):
            line = k.screen.display[y]
            if line.startswith("└"):
                break
            bars = [x for x, c in enumerate(line) if c == "│"]
            if len(bars) < 2 * pane + 2:
                continue
            lo, hi = bars[2 * pane] + 1, bars[2 * pane + 1]
            if any(k.screen.buffer[y][x].reverse for x in range(lo, hi)):
                return line[lo:hi].strip()
    return None


def selected_title(k):
    sel = selected(k, 1)
    m = re.match(r"^[▶‖■]?\s*(\w+)", sel or "")
    return m.group(1) if m else None


SCREENS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "e2e_queue_screens.txt")


def write_screens(work, states):
    """Writes the recorded screens and state files, with the temporary
    folder's name masked so reruns give comparable output."""
    def mask(text):
        # Pane titles elide the path from the left, so mask the random
        # suffix of the folder name on its own too.
        suffix = os.path.basename(work).removeprefix("kasset-e2e-queue-")
        return text.replace(work, "<work>").replace(suffix, "XXXXXXXX")

    with open(SCREENS_FILE, "w") as f:
        for label, text in SCREENS:
            f.write(f"===== {label} =====\n{mask(text)}\n")
        for label, data in states:
            f.write(f"===== {label} =====\n" + mask(data.decode(errors="replace")))
    print(f"screens: {SCREENS_FILE}")


def main():
    binary = os.path.abspath(sys.argv[1])
    work = tempfile.mkdtemp(prefix="kasset-e2e-queue-")
    # A failed run must not leave the previous run's screens looking current.
    if os.path.exists(SCREENS_FILE):
        os.remove(SCREENS_FILE)
    library = os.path.join(work, "Music")
    state_home = os.path.join(work, "state")
    os.makedirs(state_home)
    make_library(library)
    state_file = os.path.join(state_home, "kasset", "state")
    ok = False
    states = []
    try:
        ok = run(binary, library, state_home, state_file, states)
    finally:
        for p in RUNNING:
            if p.proc.poll() is None:
                p.proc.kill()
            if p.alive:
                p.finish()
            p.proc.stderr.close()
        write_screens(work, states)
        if ok:
            shutil.rmtree(work)
        else:
            print(f"kept work folder: {work}")
    sys.exit(0 if ok else 1)


def sidebar_select(k, name):
    """With the sidebar focused, move its selection to the fixed row `name`
    (Library, Artists, Albums, Queue)."""
    k.send("g")
    for _ in range(["Library", "Artists", "Albums", "Queue"].index(name)):
        k.send("j")
    k.wait_for(lambda s: (selected(k, 0) or "").lstrip("▶‖■ ").startswith(name), 3, f"sidebar on {name}")


def show(k, name):
    """Focus the sidebar, select fixed row `name` and open it."""
    k.send("\t")
    k.wait_for(lambda s: selected(k, 0) is not None, 3, "sidebar focus")
    sidebar_select(k, name)
    k.send("\r")
    k.wait_for(lambda s: selected(k, 0) is None and name in k.main_title(), 3, f"{name} shown")


def run(binary, library, state_home, state_file, states):
    k = Kasset(binary, library, state_home)
    k.record("start")
    s = k.text()
    check("sidebar lists Queue with 0 tracks under Albums", queue_count(s) == 0, sidebar_row(s, "Queue"))

    # Play One from the album First (Albums → First → One).
    show(k, "Albums")
    k.send("g\r")
    k.wait_for(lambda s: titles(s) == ["One", "Two", "Three"], 3, "First's tracks")
    k.send("g\r")
    k.wait_for(lambda s: k.playing("One"), 3, "One playing")
    k.record("One playing from the album")
    check("One plays from the album", k.now()[:3] == ("▶", "One", "Alpha"), str(k.now()))
    check("sidebar marks Albums", sidebar_marked(k.text(), "Albums"), sidebar_row(k.text(), "Albums"))

    # Queue Four, Five, Seven with e and Six with E from the library.
    show(k, "Library")
    k.send("g")
    k.send("jjj")
    k.wait_for(lambda s: selected_title(k) == "Four", 3, "Four selected")
    k.send("e")
    k.wait_for(lambda s: "Queued 'Four'" in k.status(), 3, "Four queued")
    check("e says Queued 'Four'", "Queued 'Four'" in k.status(), k.status())
    k.send("je")
    k.wait_for(lambda s: "Queued 'Five'" in k.status(), 3, "Five queued")
    k.send("jje")
    k.wait_for(lambda s: "Queued 'Seven'" in k.status(), 3, "Seven queued")
    k.send("kE")
    k.wait_for(lambda s: "Playing next: 'Six'" in k.status(), 3, "Six queued next")
    k.record("four tracks queued")
    check("E says Playing next: 'Six'", "Playing next: 'Six'" in k.status(), k.status())
    check("sidebar shows Queue with 4 tracks", queue_count(k.text()) == 4, sidebar_row(k.text(), "Queue"))
    check("bar shows e/E queue in the library", "e/E queue" in k.text().splitlines()[-1],
          k.text().splitlines()[-1])
    check("One still plays", k.playing("One"), str(k.now()))

    # The queue view lists Six, Four, Five, Seven.
    show(k, "Queue")
    k.wait_for(lambda s: titles(s) == ["Six", "Four", "Five", "Seven"], 3, "queue view")
    k.record("queue view")
    s = k.text()
    bar = s.splitlines()[-1]
    check("queue view lists Six, Four, Five, Seven", titles(s) == ["Six", "Four", "Five", "Seven"], str(titles(s)))
    check("queue view title counts 4 tracks", "4 tracks" in s.splitlines()[0], s.splitlines()[0])
    check("queue view bar", all(x in bar for x in ("Enter play", "d remove", "J/K move", "Esc sidebar")), bar)
    check("no row in the queue view is marked", all(m == " " for m, _ in main_rows(s)), str(main_rows(s)))

    # Reorder: Seven up one (K), then remove Four (d).
    k.send("G")
    k.wait_for(lambda s: selected_title(k) == "Seven", 3, "Seven selected")
    k.send("K")
    k.wait_for(lambda s: titles(s) == ["Six", "Four", "Seven", "Five"], 3, "Seven moved up")
    check("K moves Seven above Five, still selected", selected_title(k) == "Seven", str(selected(k, 1)))
    k.send("k")
    k.wait_for(lambda s: selected_title(k) == "Four", 3, "Four selected")
    k.send("d")
    k.wait_for(lambda s: titles(s) == ["Six", "Seven", "Five"], 3, "Four removed")
    k.record("queue reordered and edited")
    s = k.text()
    check("d removes Four: Six, Seven, Five", titles(s) == ["Six", "Seven", "Five"], str(titles(s)))
    check("sidebar shows Queue with 3 tracks", queue_count(s) == 3, sidebar_row(s, "Queue"))

    # n plays Six (queued); the view follows, the sidebar marks Queue.
    k.send("n")
    k.wait_for(lambda s: k.playing("Six"), 3, "Six playing")
    k.wait_for(lambda s: titles(s) == ["Seven", "Five"], 3, "view follows the queue")
    k.record("Six playing from the queue")
    s = k.text()
    check("n plays the first queued track, Six", k.now()[:2] == ("▶", "Six"), str(k.now()))
    check("queue view lists Seven, Five", titles(s) == ["Seven", "Five"], str(titles(s)))
    check("sidebar marks Queue, not Albums", sidebar_marked(s, "Queue") and not sidebar_marked(s, "Albums"),
          f"{sidebar_row(s, 'Queue')!r} {sidebar_row(s, 'Albums')!r}")

    # Six (4 s) ends: Seven plays by itself.
    k.wait_for(lambda s: k.playing("Seven"), 10, "Seven after Six ends")
    k.wait_for(lambda s: titles(s) == ["Five"], 3, "view lists Five")
    k.record("Seven playing after Six ended")
    s = k.text()
    check("Six ends and Seven plays", k.now()[:2] == ("▶", "Seven"), str(k.now()))
    check("queue view lists only Five", titles(s) == ["Five"], str(titles(s)))
    check("sidebar shows Queue with 1 track", queue_count(s) == 1, sidebar_row(s, "Queue"))

    # Markers elsewhere: the library marks Seven; the album First is not
    # marked in the album list.
    show(k, "Library")
    k.wait_for(lambda s: marker(s, "Seven") == "▶", 3, "library marker")
    k.record("library while Seven plays")
    check("library marks Seven", marker(k.text(), "Seven") == "▶", str(main_rows(k.text())))
    # "Albums" reopens its browse stack where it was left (First's tracks);
    # Backspace pops back to the album list.
    k.send("\t")
    k.wait_for(lambda s: selected(k, 0) is not None, 3, "sidebar focus")
    sidebar_select(k, "Albums")
    k.send("\r")
    k.wait_for(lambda s: "First · Alpha" in k.main_title(), 3, "First's tracks again")
    k.send("\x7f")
    k.wait_for(lambda s: k.main_title().startswith("Albums") and "Gamma" in s, 3, "album list")
    k.record("album list while Seven plays")
    s = k.text()
    first = next((l.split("│")[3] for l in body_lines(s) if len(l.split("│")) > 3 and "First" in l.split("│")[3]), "")
    check("album First is not marked while a queued track plays", first.startswith("   "), repr(first))

    # Quit while Seven plays with Five queued.
    time.sleep(2.5)
    code, err = k.quit()
    check("quit exits 0 with empty stderr", code == 0 and err == "", f"{code} {err!r}")
    with open(state_file, "rb") as f:
        data = f.read()
    states.append(("state file at the first quit", data))
    print("--- state file ---")
    print(data.decode(errors="replace"), end="")
    print("------------------")
    check("state saves the album as the source", b"source album Alpha First\n" in data)
    check("state saves One as the play order's track", b"track Alpha/First/01%20-%20One.wav\n" in data)
    check("state saves item 0", re.search(rb"\nitem 0\n", data) is not None)
    check("state saves Seven as the queued track", b"queued-track Beta/Gamma/04%20-%20Seven.wav\n" in data)
    check("state saves the queue: Five only", re.findall(rb"\nqueue (\S+)", data) == [b"Beta/Gamma/02%20-%20Five.wav"],
          str(re.findall(rb"\nqueue (\S+)", data)))
    pos = re.search(rb"position-ms (\d+)", data)
    check("state saves Seven's position (at least 2 s)", pos is not None and int(pos.group(1)) >= 2000,
          pos.group(1).decode() if pos else "none")

    # Restart: Seven paused, Five queued, Queue marked.
    k = Kasset(binary, library, state_home)
    k.record("restart")
    s = k.text()
    check("restart resumes Seven paused", k.now() is not None and k.now()[:2] == ("‖", "Seven"), str(k.now()))
    secs = re.search(r"(\d+):(\d\d) ━", s)
    check("resumed at the saved position", secs is not None and int(secs.group(1)) * 60 + int(secs.group(2)) >= 2,
          secs.group(0) if secs else s.splitlines()[-3])
    check("restart restores the queue: 1 track", queue_count(s) == 1, sidebar_row(s, "Queue"))
    check("sidebar marks Queue after restart", sidebar_marked(s, "Queue") and not sidebar_marked(s, "Albums"),
          sidebar_row(s, "Queue"))
    show(k, "Queue")
    check("restored queue lists Five", titles(k.text()) == ["Five"], str(titles(k.text())))

    # n: Five (queued); n: the album continues with Two.
    k.send("n")
    k.wait_for(lambda s: k.playing("Five"), 3, "Five playing")
    k.wait_for(lambda s: "Queue is empty" in s, 3, "queue emptied")
    k.record("Five playing after restart")
    s = k.text()
    check("n plays the queued Five", k.now()[:2] == ("▶", "Five"), str(k.now()))
    check("queue is empty", queue_count(s) == 0 and "Queue is empty" in s, sidebar_row(s, "Queue"))
    k.send("n")
    k.wait_for(lambda s: k.playing("Two"), 3, "Two playing")
    k.record("album continues with Two")
    s = k.text()
    check("the album continues with Two", k.now()[:3] == ("▶", "Two", "Alpha"), str(k.now()))
    check("sidebar marks Albums again, not Queue", sidebar_marked(s, "Albums") and not sidebar_marked(s, "Queue"),
          f"{sidebar_row(s, 'Albums')!r} {sidebar_row(s, 'Queue')!r}")
    code, err = k.quit()
    check("second quit exits 0", code == 0 and err == "", f"{code} {err!r}")
    with open(state_file, "rb") as f:
        final = f.read()
    states.append(("state file at the second quit", final))
    check("final state has no queue lines", b"\nqueue " not in final and b"queued-track" not in final)

    failed = [n for n, ok in RESULTS if not ok]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return not failed


if __name__ == "__main__":
    main()
