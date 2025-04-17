# Welcome to the RhythmiRust-Wiki Repository!

Thank you for visiting the **RhythmiRust-Wiki** repository! This project is dedicated to providing a comprehensive wiki for RhythmiRust.

## Overview

This repository contains:
- A [Compiler](./wiki_compiler) that converts SVG (Scalable Vector Graphics) files into PNG (Portable Network Graphics) format, copies and optimises other files to generate the final wiki directory
- JSON files that store wiki pages/layouts, which my program uses to render content into wiki pages.
- Images referenced in the JSON files, allowing the wiki to display them appropriately within the rendered pages.

## Contributing

We welcome contributions from everyone! If you find an area that could be improved—whether it's rewording a section for clarity, expanding on a topic, or adding new content—please feel free to edit and enhance the wiki. Your contributions will help create a richer resource for all users.

## Getting Started

To get started with the RhythmiRust-Wiki:
1. Clone the repository to your local machine.
2. Navigate to the [Compiler](./wiki_compiler) and compile it using [Rust](https://www.rust-lang.org/tools/install) Commands given in [Quick Start](#quick-start).
2. Explore [**_Wiki_build_helper.json**](./_Wiki_build_helper.json) to see example building blocks of the wiki.
3. Copy/Reference from [**_Wiki_build_helper.json**](./_Wiki_build_helper.json) and design the page as you see fit.

## Quick Start

> [!IMPORTANT]
> NOTE: This assumes you have [Rust](https://www.rust-lang.org/tools/install) and [git](https://git-scm.com/downloads) installed and available in your $PATH environmental variables.

Setup and generate the wiki:

**Bash/Linux**
```bash
git clone https://github.com/UnknownSuperficialNight/RhythmiRust-Wiki
cd RhythmiRust-Wiki/wiki_compiler
cargo build --release
mv "./target/release/compile_wiki" "../compile_wiki"
cargo clean
cd ..
./compile_wiki
```

**Powershell/Windows**
```powershell
git clone https://github.com/UnknownSuperficialNight/RhythmiRust-Wiki.git
Set-Location RhythmiRust-Wiki\wiki_compiler
cargo build --release
Move-Item ".\target\release\compile_wiki.exe" "../compile_wiki.exe"
cargo clean
Set-Location ..
./compile_wiki.exe
```

Now the production wiki should have been generated in the `./Wiki` directory.

## License

This project is licensed under the **BSD 3-Clause License**. See the [LICENSE](./LICENSE) file for more details.

## Thank You!

I appreciate your interest in the RhythmiRust-Wiki. Happy coding!
