-- Module-time randomness is seeded before source execution in capture mode.
return { r = 0.3 + math.random() * 0.2, g = 0.8, b = 0.65 }
