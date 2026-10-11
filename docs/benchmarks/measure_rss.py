import json, resource, subprocess, sys
result = subprocess.run([sys.argv[1]], stdout=subprocess.DEVNULL, check=True)
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
print(json.dumps({'binary': sys.argv[1], 'platform': sys.platform, 'max_rss_bytes_on_macos': usage.ru_maxrss, 'user_seconds': usage.ru_utime, 'system_seconds': usage.ru_stime}))
