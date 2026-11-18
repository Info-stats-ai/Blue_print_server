# Out of Sync — video generation script

13 clips, about 7 seconds each, about 90 seconds in total. Each prompt below
stands on its own, because video APIs don't remember earlier clips. Paste the
STYLE block, then the descriptions of the characters in that shot, then the
shot prompt.

## How to get a good result

1. **Make keyframes first, then animate them.** Generate one still image per
   shot with an image model and pick the best. Then send that image plus the
   shot prompt to the video model ("image-to-video"). This keeps faces and
   clothes the same from shot to shot.
2. **Keep on-screen text off the generator.** Video models garble words. The
   prompts say "screen glows, no readable text". Add the real words ("Cart: 1
   item", "Shipping is free over $50") in the edit, or lay real app screenshots
   from `site/docs-img/` over the screens.
3. **Record the voices separately.** Generate narration and dialogue with a
   text-to-speech API using the lines below, then lay them over the clips. If
   your video model makes its own audio, add the line in quotes at the end of
   the prompt.
4. **Use the negative prompt on every clip** if the API accepts one.

## STYLE (paste first in every prompt)

> Modern Japanese-style 2D anime, original characters, clean line art, soft
> cel shading, gentle film grain, 16:9, 24 fps, cinematic framing, a modern
> open-plan tech office with tall windows and a city skyline.

**Before palette (shots 1–6):** cool grey-blue, dim monitor glow, night.
**After palette (shots 7–13):** warm amber and gold, late-afternoon sun.

## NEGATIVE PROMPT (every clip)

> readable text, logos, brand names, watermark, subtitles, extra fingers,
> distorted faces, photorealistic, 3D render, chibi, existing anime characters

## CHARACTERS (paste the ones that appear in the shot, word for word)

- **MINA (team lead):** a woman in her early 30s, short straight black bob,
  calm dark eyes, cream cardigan over a white shirt, holds a white mug.
- **RAVI (backend):** a man in his late 20s, warm brown skin, short wavy black
  hair, charcoal hoodie, over-ear headphones round his neck, two monitors.
- **SORA (data):** a woman in her mid 20s, long dark-brown ponytail, round
  glasses, olive bomber jacket, laptop covered in colorful stickers.
- **JUN (front end):** a man in his mid 20s, messy light-brown hair, navy
  shirt with the sleeves rolled up, stands at a standing desk with a sketchbook.

---

## Part 1 — Before (grey-blue, slow)

### Shot 1 · 7s · Night office
**Prompt:** Wide establishing shot of the office at night. Four desks, each lit
by a monitor. RAVI, SORA and JUN are slumped in their chairs, exhausted, and
MINA stands at the back. A round wall clock shows a quarter to ten. Rain
streaks the windows. Slow push-in. Cool grey-blue palette.
**Characters:** all four.
**Narration:** "Four people. One product. Four different versions of it."

### Shot 2 · 6s · Ravi's screen
**Prompt:** Medium shot over RAVI's shoulder. Both monitors fill with red error
blocks (glowing red shapes, no readable text). He leans back, rubs his eyes
with both hands, then drops his hands in frustration. Grey-blue palette, red
glow on his face.
**Characters:** RAVI.
**Ravi (tired):** "Who changed the cart again?"

### Shot 3 · 7s · Sora, unaware
**Prompt:** Medium close-up of SORA in headphones, nodding to music, typing
happily. A cheerful green chat bubble pops up on her laptop. Behind her, out of
focus, RAVI is glaring at his red screens. She has no idea. Grey-blue palette.
**Characters:** SORA, RAVI (background).
**Narration:** "Every AI only knew one person's half."

### Shot 4 · 6s · Jun's broken page
**Prompt:** JUN at his standing desk clicks a button on a web page twice. The
small counter in the corner of the page doesn't change. He stares, deadpan,
then lets his head fall back and sighs at the ceiling. Grey-blue palette.
**Characters:** JUN.
**Jun (flat):** "It worked on my machine."

### Shot 5 · 7s · Nobody looks up
**Prompt:** MINA walks slowly between the desks holding her mug. Behind her is
a wall of sticky notes, half of them crossed out. She opens her mouth to ask
something, but RAVI, SORA and JUN all stay hunched over their screens and none
of them look up. Slow sideways tracking shot. Grey-blue palette.
**Characters:** all four.
**Mina (quietly):** "Where are we?"

### Shot 6 · 6s · Push, pull, wait
**Prompt:** Extreme close-up of a team chat window. Message bubbles stack up
faster and faster until they blur into streaks of light. The camera whips
sideways into darkness. Grey-blue palette, rising tension.
**Characters:** none.
**Narration:** "Push. Pull. Wait. Repeat."
**In the edit:** overlay the bubbles "pushed", "pull first", "wait, which branch?"

---

## Part 2 — The switch (one beat of silence, then warm color)

### Shot 7 · 8s · Mina opens I
**Prompt:** Close-up of MINA opening a laptop at her desk. The screen's glow
lights her face, and warm golden light spreads across the room from the
screen, replacing the grey-blue. She smiles slightly and clicks once. Soft
sparkles drift up. Palette shifts from cool to warm during the shot.
**Characters:** MINA.
**Sound:** one second of silence, then a soft chime on the click.
**In the edit:** lay the real I home screen over the laptop screen.

### Shot 8 · 7s · Invites fly
**Prompt:** Three glowing paper planes fly from MINA's laptop across the
office, each landing on a different monitor. RAVI, SORA and JUN each look up,
surprised, and click. Their screens light up warm gold one after another. Wide
shot, dynamic camera following the planes. Warm palette.
**Characters:** all four.
**Sound:** three small chimes as each plane lands.

---

## Part 3 — After (warm, fast cuts, the music lifts)

### Shot 9 · 7s · One decision, everywhere
**Prompt:** Four-way split screen showing MINA, RAVI, SORA and JUN at their
desks. MINA taps her screen, and a small glowing note appears on all four
screens at the same moment, with a gentle pulse of light. All three look up
and nod. Warm palette.
**Characters:** all four.
**Narration:** "One decision. Everyone, and every AI, knows it."
**In the edit:** overlay "Shipping is free over $50" on the note.

### Shot 10 · 7s · In sync
**Prompt:** RAVI and SORA at neighboring desks. Code flows onto both of their
screens at once, and small green checkmark tags pop up one after another.
There's no red anywhere. SORA turns to RAVI, grinning, and points at her
screen. RAVI laughs. Warm afternoon light.
**Characters:** RAVI, SORA.
**Sora (grinning):** "It already knows Ravi renamed it."

### Shot 11 · 7s · Jun's fix
**Prompt:** JUN watches a small browser window inside his code editor. A
cursor clicks a button twice, the counter on the page updates correctly, and a
green check appears. JUN pumps his fist in the air, delighted. Warm palette,
energetic.
**Characters:** JUN.
**In the edit:** "Cart: 1 item" changes to "Cart: 2 items".

### Shot 12 · 7s · The lead watches
**Prompt:** MINA leans back in her chair with her white mug, relaxed. On her
screen, a side panel lists four people, each with a small live progress line
ticking forward. Golden sunset light through the windows. Slow push-in on her
content smile. She takes a sip.
**Characters:** MINA.
**Narration:** "The lead sees everything as it happens."
**In the edit:** overlay "ship it" on the mug, and the real Team activity
screenshot on her screen.

### Shot 13 · 9s · Home on time
**Prompt:** MINA clicks three times and three green checkmarks appear. Cut to
a wide shot of the office at sunset: the wall clock shows a little after six,
and RAVI, SORA and JUN pack their bags, laughing and waving to each other. The
camera pulls back slowly through the window to the golden city skyline.
**Characters:** all four.
**Narration:** "Less waiting. More building. I — every AI in sync."
**End card (in the edit, 3s):** the I logo on dark, then "Build together."

---

## Music (for a music API or a stock track)

- **Shots 1–6:** slow lo-fi piano, about 70 BPM, melancholy, with keyboard
  clicks and a phone buzzing.
- **Shots 7–8:** one beat of silence, then a soft chime, then the music builds.
- **Shots 9–13:** upbeat lo-fi with light drums, about 95 BPM, hopeful. A
  small chime on each green check.

## Full narration (for one text-to-speech pass)

Voice: calm, warm, mid-paced, friendly. Leave a short pause between lines.

> Four people. One product. Four different versions of it.
> Every AI only knew one person's half.
> Push. Pull. Wait. Repeat.
> One decision. Everyone, and every AI, knows it.
> The lead sees everything as it happens.
> Less waiting. More building. I — every AI in sync.
