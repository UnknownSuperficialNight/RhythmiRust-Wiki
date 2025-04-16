import os
import shutil
import subprocess

def recreate_directory_structure(source_dir, target_dir):
    # Remove the target directory if it exists
    if os.path.exists(target_dir):
        shutil.rmtree(target_dir)

    # Walk through the source directory
    for root, dirs, files in os.walk(source_dir):
        # Skip .git directory
        if '.git' in dirs:
            dirs.remove('.git')  # This prevents os.walk from traversing into .git

        # Determine the relative path from the source directory
        relative_path = os.path.relpath(root, source_dir)
        # Create the corresponding directory in the target directory
        target_path = os.path.join(target_dir, relative_path)
        os.makedirs(target_path, exist_ok=True)

        # Copy data.json files, PNG files, and convert ODG files
        for file in files:
            source_file_path = os.path.join(root, file)
            if file == 'data.json':
                # Copy data.json files
                shutil.copy2(source_file_path, target_path)
            elif file.endswith('.png'):
                # Copy PNG files to the target directory
                shutil.copy2(source_file_path, target_path)
            elif file.endswith('.odg'):
                # Convert ODG files to PNG
                convert_odg_to_png(source_file_path, target_path)

def convert_odg_to_png(odg_file_path, output_dir):
    # Use LibreOffice to convert .odg to .png (i've tried to make this multi-threaded however you cannot have more than one libreoffice at a time.)
    subprocess.run([
        'libreoffice', '--headless', '--convert-to', 'png',
        '--outdir', output_dir, odg_file_path
    ], check=True)

if __name__ == "__main__":

    # Define the source and target directories using relative paths
    wiki_source_dir = os.path.dirname(os.path.abspath(__file__)) # Get the current script directory
    wiki_dir = os.path.join(wiki_source_dir, '..', 'Wiki')  # One level up, in 'wiki'

    recreate_directory_structure(wiki_source_dir, wiki_dir)
