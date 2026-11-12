import os, subprocess, sys, bisect
from PIL import Image, ImageDraw, ImageFont

take, voice, out = sys.argv[1], sys.argv[2], sys.argv[3]
FPS, W, H = 20, 1920, 1080
BG = (11, 13, 16)

# (seconds on screen, Alice source start/end, Bob source start/end, caption)
SCENES = [
    (5.0, 0.1, 5.6, 0.1, 5.6, "This is I: a code editor where a team builds together,\nand everyone has their own AI."),
    (5.5, 5.6, 17.5, 5.6, 17.5, "Alice and Bob are in one session.\nThey chat right beside their code."),
    (9.0, 17.5, 43.0, 17.5, 43.0, "Alice switches to Plan mode. Her AI reads the project\nand writes a plan, with an owner for every step."),
    (6.5, 43.0, 84.0, 43.0, 84.0, "Meanwhile, Bob is already coding.\nAlice can see where he is, live."),
    (7.0, 84.0, 100.5, 84.0, 100.5, "Alice checks the plan, and shares it\nwith the team in one click."),
    (9.0, 100.5, 134.0, 100.5, 134.0, "The plan becomes tasks. Team activity shows each task\nand its owner, on both screens."),
    (6.0, 134.0, 144.0, 134.0, 144.0, "When they save, their code syncs,\nand every change is tracked."),
    (9.0, 150.9, 158.8, 144.0, 144.0, "And you stay in control: you choose what gets shared.\nI. Build it together."),
]

def load(side):
    names = sorted(n for n in os.listdir(f"{take}/{side}") if n.endswith(".jpg"))
    return [int(n[:-4]) / 1000 for n in names], names
frames = {side: load(side) for side in "ab"}

WIN_W = 940
cache = {}
patch = None
def window(side, t):
    global patch
    times, names = frames[side]
    i = max(0, bisect.bisect_right(times, t) - 1)
    key = (side, i)
    if key not in cache:
        if len(cache) > 6:
            cache.clear()
        im = Image.open(f"{take}/{side}/{names[i]}").convert("RGB")
        if side == "b" and times[i] < 11.6:
            # A hover tooltip over the file list early on; cover it with the
            # same area from a moment later.
            if patch is None:
                j = bisect.bisect_right(times, 12.2)
                patch = Image.open(f"{take}/b/{names[j]}").convert("RGB").crop((0, 60, 1300, 420))
            im.paste(patch, (0, 60))
        h = round(im.height * WIN_W / im.width)
        cache[key] = im.resize((WIN_W, h), Image.LANCZOS)
    return cache[key]

font = lambda size, index=0: ImageFont.truetype("/System/Library/Fonts/HelveticaNeue.ttc", size, index=index)
label_font, caption_font, small_font = font(30, 1), font(34), font(22)

def base():
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    for x, name, colour, note in ((14, "Alice", (167, 139, 250), "leads the session"), (966, "Bob", (190, 220, 110), "teammate")):
        d.ellipse((x + 6, 30, x + 24, 48), fill=colour)
        d.text((x + 36, 20), name, font=label_font, fill=(235, 238, 242))
        d.text((x + 36 + d.textlength(name, font=label_font) + 14, 28), note, font=small_font, fill=(130, 138, 150))
    d.text((W - 14 - d.textlength("i-ide.dev", font=small_font), 28), "i-ide.dev", font=small_font, fill=(130, 138, 150))
    return im
BASE = base()

total = sum(scene[0] for scene in SCENES)
starts = []
t = 0
for scene in SCENES:
    starts.append(t); t += scene[0]

audio_inputs, filters = [], []
for k, start in enumerate(starts):
    audio_inputs += ["-i", f"{voice}/n{k+1}.aiff"]
    filters.append(f"[{k+1}:a]adelay={int((start+0.35)*1000)}:all=1[a{k}]")
mix = "".join(f"[a{k}]" for k in range(len(starts))) + f"amix=inputs={len(starts)}:normalize=0,apad=whole_dur={total}[aout]"
cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
       *audio_inputs, "-filter_complex", ";".join(filters) + ";" + mix, "-map", "0:v", "-map", "[aout]",
       "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-r", "30",
       "-c:a", "aac", "-b:a", "160k", "-ar", "44100", "-ac", "2", "-t", str(total), "-movflags", "+faststart", out]
proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)

n_frames = int(total * FPS)
for f in range(n_frames):
    t = f / FPS
    k = max(i for i, s in enumerate(starts) if s <= t)
    dur, a0, a1, b0, b1, caption = SCENES[k]
    p = (t - starts[k]) / dur
    im = BASE.copy()
    a = window("a", a0 + (a1 - a0) * p); b = window("b", b0 + (b1 - b0) * p)
    im.paste(a, (14, 70)); im.paste(b, (966, 70))
    d = ImageDraw.Draw(im)
    y = 70 + a.height + 14
    lines = caption.split("\n")
    for n, line in enumerate(lines):
        w = d.textlength(line, font=caption_font)
        d.text(((W - w) / 2, y + n * 44), line, font=caption_font, fill=(235, 238, 242))
    # fade in and out
    fade = min(1.0, t / 0.4, (total - t) / 0.6)
    if fade < 1.0:
        im = Image.blend(Image.new("RGB", (W, H), BG), im, max(0.0, fade))
    proc.stdin.write(im.tobytes())
proc.stdin.close()
print("ffmpeg exit", proc.wait(), "length", total)
