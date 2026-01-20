# Multi-Threaded Crawler Update

## Summary of Changes

Successfully refactored the NGA BBS API crawler to automatically fetch all pages using multi-threading.

---

## What Changed

### 1. **Removed Manual Page Arguments**

**Before:**
```bash
python nga_crawler.py --tid 12345 --page 1
python nga_crawler.py --tid 12345 --start-page 1 --end-page 5
```

**After:**
```bash
python nga_crawler.py --tid 12345  # Automatically fetches ALL pages
```

### 2. **Added Automatic Page Discovery**

The crawler now:
1. Fetches page 1 first
2. Extracts `totalPage` from the response
3. Automatically crawls pages 2 through `totalPage`

### 3. **Implemented Multi-Threading**

- Uses Python's `ThreadPoolExecutor` for concurrent page fetching
- Thread count is configurable via `max_threads` in config file
- Default: 5 concurrent threads
- Threads fetch pages in parallel for much faster crawling

### 4. **Enhanced Progress Reporting**

New output includes:
- Thread metadata (title, total pages, total posts)
- Real-time progress tracking
- Per-page completion status with ✓/✗ indicators
- Final summary with success/failure counts

### 5. **Added Configuration Option**

New field in `config.json`:
```json
{
  "max_threads": 5
}
```

This controls how many pages are fetched concurrently.

---

## Technical Implementation

### New Method: `crawl_all_pages()`

```python
def crawl_all_pages(self, tid: int) -> List[Dict[str, Any]]:
    # 1. Fetch page 1 to get totalPage
    first_page = self.fetch_page(tid, 1)
    total_pages = first_page.get('totalPage', 1)
    
    # 2. Use ThreadPoolExecutor for remaining pages
    with ThreadPoolExecutor(max_workers=max_threads) as executor:
        future_to_page = {
            executor.submit(self.fetch_page, tid, page): page 
            for page in range(2, total_pages + 1)
        }
        
        # 3. Collect results as they complete
        for future in as_completed(future_to_page):
            result = future.result()
            all_results.append(result)
    
    # 4. Sort and return all results
    all_results.sort(key=lambda x: x.get('currentPage', 0))
    return all_results
```

### Key Features

- **Sequential page 1**: Always fetched first to get metadata
- **Parallel remaining pages**: Pages 2+ fetched concurrently
- **Thread pool**: Reuses threads efficiently
- **Result sorting**: Ensures pages are displayed in order despite parallel fetching
- **Error handling**: Individual page failures don't stop the entire crawl

---

## Performance Impact

### Before (Sequential)
- **181 pages** × **~1 second per request** = **~181 seconds** (~3 minutes)

### After (5 concurrent threads)
- **Page 1**: ~1 second
- **Pages 2-181** (180 pages / 5 threads): ~36 seconds
- **Total**: **~37 seconds** 

**Speed improvement: ~5× faster!**

With more threads:
- **10 threads**: ~18 seconds total (**10× faster**)
- **20 threads**: ~9 seconds total (**20× faster**)

*Note: Higher thread counts may trigger rate limiting from the server.*

---

## Sample Output

```
================================================================================
Starting crawl for thread 45974302
================================================================================

[1/3] Fetching page 1 to determine total pages...
✓ Thread: 自立自强,科学技术打头阵
✓ Total pages: 181
✓ Total posts: 3615
✓ Posts per page: 20

[2/3] Fetching pages 2-181 using 5 threads...
  ✓ Page 2/181 completed
  ✓ Page 3/181 completed
  ✓ Page 5/181 completed
  ✓ Page 4/181 completed
  ✓ Page 6/181 completed
  ✓ Page 7/181 completed
  ... (175 more pages)

[3/3] Crawl summary:
  ✓ Successful: 181/181 pages

================================================================================
Page 1
================================================================================
{ ... JSON output ... }

================================================================================
Page 2
================================================================================
{ ... JSON output ... }

...

================================================================================
Crawl completed: 181/181 pages retrieved
================================================================================
```

---

## Configuration Recommendations

### Conservative (Polite to Server)
```json
{
  "max_threads": 2
}
```
- Slower but very polite
- Minimal server load
- Best for continuous monitoring

### Balanced (Recommended)
```json
{
  "max_threads": 5
}
```
- Good balance of speed and politeness
- Default setting
- Suitable for most use cases

### Aggressive (Fast Crawling)
```json
{
  "max_threads": 10
}
```
- Very fast crawling
- May trigger rate limiting
- Use sparingly

---

## Backward Compatibility

The old page-based arguments have been completely removed. Users must update their scripts:

**Old usage (no longer works):**
```bash
python nga_crawler.py --tid 12345 --page 1  # ERROR
```

**New usage:**
```bash
python nga_crawler.py --tid 12345  # Fetches ALL pages automatically
```

---

## Files Modified

1. **nga_crawler.py**
   - Removed page CLI arguments
   - Added `crawl_all_pages()` method
   - Added `_print_result()` helper
   - Implemented ThreadPoolExecutor logic
   - Enhanced progress reporting

2. **config.example.json**
   - Added `max_threads` field

3. **README.md**
   - Updated usage examples
   - Added "How It Works" section
   - Added "Performance Tuning" section
   - Updated sample output

---

## Next Steps

Potential enhancements:
1. **Save to files**: Export results to JSON/CSV instead of stdout
2. **Incremental crawling**: Only fetch new pages since last run
3. **Filter on fetch**: Filter by author/votes during crawling
4. **Database storage**: Store results in SQLite/PostgreSQL
5. **Progress bar**: Add visual progress indicator (e.g., tqdm)
6. **Rate limiting**: Add configurable delays between requests
7. **Resume capability**: Save progress and resume interrupted crawls
