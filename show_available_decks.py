#!/usr/bin/env python3
"""
Show what deck options would be available during match creation.
"""

import os

def main():
    definitions_dir = 'definitions'

    all_decks = []

    # Load from unified definitions
    for filename in sorted(os.listdir(definitions_dir)):
        if not filename.endswith('.toml'):
            continue

        filepath = os.path.join(definitions_dir, filename)
        with open(filepath, 'r') as f:
            content = f.read()

        # Parse TOML to find deck names
        import re

        # Get archetype name
        name_match = re.search(r'^name = "(.*?)"', content, re.MULTILINE)
        if not name_match:
            continue

        archetype_name = name_match.group(1)

        # Check for versions first (priority over subtypes/archetypes)
        in_versions = False
        for line in content.split('\n'):
            if line.strip() == '[versions]':
                in_versions = True
                continue
            if in_versions:
                if line.strip().startswith('['):
                    # Exited versions section
                    in_versions = False
                    continue
                # Parse version entries like "Tempo-2.1" = "url"
                version_match = re.match(r'^"(.*?)" = ', line)
                if version_match:
                    all_decks.append(version_match.group(1))

        # Check for subtypes
        subtype_matches = re.findall(r'^\[subtypes\.(.*?)\]', content, re.MULTILINE)

        if subtype_matches:
            # Has subtypes - add each variant
            for subtype in subtype_matches:
                # Remove quotes if present
                subtype = subtype.strip('"')
                all_decks.append(f"{archetype_name}: {subtype}")
        else:
            # No subtypes - add archetype directly
            all_decks.append(archetype_name)

    # Sort and display
    all_decks.sort()

    # Move "Other" to end
    if "Other" in all_decks:
        all_decks.remove("Other")
        all_decks.append("Other")

    print(f"Available decks during match creation ({len(all_decks)} options):")
    print("=" * 60)
    for i, deck in enumerate(all_decks, 1):
        print(f"{i:2d}. {deck}")

if __name__ == '__main__':
    main()
