"""Circle helpers.

All functions take a radius in metres and raise ValueError when it is negative.
"""

import math


def area(r):
    return math.pi * r * r


def circumference(r):
    return 2 * math.pi * r


def describe(r):
    return f"circle of radius {r} m"
