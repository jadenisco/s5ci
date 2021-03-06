package main

import (
	"crypto/sha1"
	"encoding/json"
	"fmt"
	mustache "github.com/hoisie/mustache"
	"gopkg.in/yaml.v2"
	"io"
	"io/ioutil"
	"log"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"strings"
)

func ensureDbPath(job *Job) {
	job_data_dir := JobGetDataPathFromJobId(job.Job_ID)
	err := os.MkdirAll(job_data_dir, 0755)
	if err != nil {
		log.Fatal(err)
	}
}

func SaveJobYaml(job *Job) {
	c := S5ciOptions.Config
	d, err := yaml.Marshal(job)
	if err != nil {
		log.Fatalf("error: %v", err)
	}
	// write to both places for now
	writeToFileIfDifferent(filepath.Join(c.Jobs.Rootdir, job.Job_ID, "job.yaml"), string(d))
	ensureDbPath(job)
	writeToFileIfDifferent(filepath.Join(JobGetDataPathFromJobId(job.Job_ID), "job.yaml"), string(d))
}
func SaveJobJson(job *Job) {
	c := S5ciOptions.Config
	job_lowercase := structToLowerMap(*job)
	d, err := json.Marshal(job_lowercase)
	if err != nil {
		log.Fatalf("error: %v", err)
	}
	// write to both places for now
	writeToFileIfDifferent(filepath.Join(c.Jobs.Rootdir, job.Job_ID, "job.json"), string(d))
	ensureDbPath(job)
	writeToFileIfDifferent(filepath.Join(JobGetDataPathFromJobId(job.Job_ID), "job.json"), string(d))
}

var compiled_templates map[string]*mustache.Template = make(map[string]*mustache.Template)

func compileTemplate(template_name string) (*mustache.Template, error) {
	if compiled_templates[template_name] != nil {
		return compiled_templates[template_name], nil
	} else {
		template, err := compileTemplateUncached(template_name)
		if err != nil {
			log.Fatalf("error compiling template: %v", err)
		}
		compiled_templates[template_name] = template
		return template, nil
	}
}

func compileTemplateUncached(template_name string) (*mustache.Template, error) {
	c := S5ciOptions.Config
	fname := fmt.Sprintf("%s.mustache", template_name)
	full_name := filepath.Join(c.Install_Rootdir, "templates", fname)
	return mustache.ParseFile(full_name)
}

// WriteToFile will print any string of text to a file safely by
// checking for errors and syncing at the end.
func writeToFile(filename string, data string) error {
	file_dir := filepath.Dir(filename)
	os.MkdirAll(file_dir, 0755)
	file, err := os.Create(filename)
	if err != nil {
		return err
	}
	defer file.Close()

	_, err = io.WriteString(file, data)
	if err != nil {
		return err
	}
	return file.Sync()
}

var sum_exclude_regex = regexp.MustCompile(`(?s)START EXCLUDE FROM CSUM.*?END EXCLUDE FROM CSUM`)

func writeToFileIfDifferent(fname string, data string) {
	// calculate sha1 of the file
	sum_of_file := "0"
	f_data_b, err := ioutil.ReadFile(fname)
	if err == nil {
		f_data := string(f_data_b)
		h := sha1.New()
		f_data_comp := sum_exclude_regex.ReplaceAllString(f_data, "")
		io.WriteString(h, f_data_comp)
		sum_of_file = fmt.Sprintf("%x", h.Sum(nil))
	}

	// calculate the sum of bytes
	h := sha1.New()
	data_comp := sum_exclude_regex.ReplaceAllString(data, "")
	io.WriteString(h, data_comp)
	sum_of_string := fmt.Sprintf("%x", h.Sum(nil))

	if sum_of_string != sum_of_file {
		fmt.Println("Saving ", fname)
		writeToFile(fname, data)
	}

}

var batch_regenerate bool = false
var batch_pending_groups map[string]interface{} = make(map[string]interface{})

func BatchHtmlRegenerateStart() {
	batch_regenerate = true
}

func BatchHtmlRegenerateFinish() {
	// reset batch regenerate if it was set
	batch_regenerate = false
	for group_name, _ := range batch_pending_groups {
		fmt.Printf("Regenerating group %s\n", group_name)
		ReallyRegenerateGroupHtml(group_name)
	}
	ReallyRegenerateActiveHtml()
	ReallyRegenerateRootHtml()
	batch_pending_groups = make(map[string]interface{})
}

func RegenerateRootHtml() {
	if !batch_regenerate {
		ReallyRegenerateRootHtml()
	}
}

func RegenerateGroupHtml(group_name string) {
	if batch_regenerate {
		batch_pending_groups[group_name] = true
	} else {
		ReallyRegenerateGroupHtml(group_name)
	}
}

func RegenerateActiveHtml() {
	if !batch_regenerate {
		ReallyRegenerateActiveHtml()
	}
}

const jobs_per_page = 50

func ReallyRegenerateRootHtml() {
	c := S5ciOptions.Config
	template, err := compileTemplate("root_job_page")
	if err != nil {
		log.Fatal(err)
	}
	db := DbOpen()
	defer DbClose(&db)

	data := make(map[string]interface{})

	out_cjs := make([]map[string]interface{}, jobs_per_page)
	iter, err := db.db.Model(&Job{}).Order("started_at desc").Rows()
	i := 0
	for iter.Next() {
		var elem Job
		if i < jobs_per_page {
			db.db.ScanRows(iter, &elem)
			out_cjs[i] = structToLowerMap(elem)
			i = i + 1
		}
	}
	//      data["prev_page_name"] = "index_full.html"
	data["child_jobs"] = out_cjs
	data["now"] = S5Now()
	rtdt := &S5ciRuntime
	data["hostname"] = rtdt.Hostname
	fname := filepath.Join(c.Jobs.Rootdir, "index.html")
	writeToFileIfDifferent(fname, template.Render(&data))
}

func ReallyRegenerateActiveHtml() {
	c := S5ciOptions.Config
	template, err := compileTemplate("active_job_page")
	if err != nil {
		log.Fatal(err)
	}
	data := make(map[string]interface{})
	cjs := DbGetActiveJobs()
	out_cjs := make([]map[string]interface{}, len(cjs))
	for i, elem := range cjs {
		out_cjs[i] = structToLowerMap(elem)
	}
	data["child_jobs"] = out_cjs
	data["now"] = S5Now()
	rtdt := &S5ciRuntime
	data["hostname"] = rtdt.Hostname
	writeToFileIfDifferent(filepath.Join(c.Jobs.Rootdir, "active.html"), template.Render(&data))
}

func ReallyRegenerateGroupHtml(group_name string) {
	c := S5ciOptions.Config
	template, err := compileTemplate("group_job_page")
	if err != nil {
		log.Fatal(err)
	}
	data := make(map[string]interface{})

	cjs := DbGetJobsByGroupName(group_name)
	out_cjs := make([]map[string]interface{}, len(cjs))
	for i, elem := range cjs {
		out_cjs[i] = structToLowerMap(elem)
	}
	data["child_jobs"] = out_cjs
	data["job_group_name"] = group_name
	data["now"] = S5Now()
	rtdt := &S5ciRuntime
	data["hostname"] = rtdt.Hostname
	writeToFileIfDifferent(filepath.Join(c.Jobs.Rootdir, group_name, "index.html"), template.Render(&data))
}

func structToLowerMap(in interface{}) map[string]interface{} {
	v := reflect.ValueOf(in)
	vType := v.Type()

	result := make(map[string]interface{}, v.NumField())

	for i := 0; i < v.NumField(); i++ {
		name := vType.Field(i).Name
		// fmt.Printf("%d: %s : %s\n", i, name, reflect.ValueOf(v.Field(i).Interface()).Kind())
		if reflect.ValueOf(v.Field(i).Interface()).Kind() == reflect.Ptr {
			if v.Field(i).IsNil() {
				result[strings.ToLower(name)] = nil
			} else {
				result[strings.ToLower(name)] = reflect.Indirect(v.Field(i)).Interface()
			}
		} else {
			result[strings.ToLower(name)] = v.Field(i).Interface()
		}
	}
	result["short_job_id"] = JobShortenJobId(result["job_id"].(string))
	result["short_instance_id"] = JobShortenInstanceId(result["instance_id"].(string))
	return result
}

func regenerateHtml(job_id string, update_parent bool, update_children bool, groups *map[string]int) {
	c := S5ciOptions.Config
	j, err := DbGetJob(job_id)
	if err != nil {
		log.Fatal(err)
	}
	template, err := compileTemplate("job_page")
	if err != nil {
		log.Fatal(err)
	}
	SaveJobYaml(j)
	SaveJobJson(j)

	data := make(map[string]interface{})

	data["job"] = structToLowerMap(*j)

	var pj *Job = nil
	if j.Parent_Job_ID != nil {
		pj, _ = DbGetJob(*j.Parent_Job_ID)
		if pj != nil {
			data["parent_job"] = structToLowerMap(*pj)
		}
	}
	cjs := DbGetChildJobs(job_id)

	out_cjs := make([]map[string]interface{}, len(cjs))
	for i, elem := range cjs {
		out_cjs[i] = structToLowerMap(elem)
	}
	data["child_jobs"] = out_cjs
	data["now"] = S5Now()
	rtdt := &S5ciRuntime
	data["hostname"] = rtdt.Hostname

	// archive_dir_name := filepath.Join(c.Jobs.Rootdir, job_id, "archive")
	// if s, err := os.Stat(archive_dir_name); err == nil && s.IsDir() {
	if j.Finished_At != nil {
		data["archive_dir"] = "archive"
	}
	writeToFileIfDifferent(filepath.Join(c.Jobs.Rootdir, job_id, "index.html"), template.Render(&data))

	if update_children {
		for _, cj := range cjs {
			regenerateHtml(cj.Job_ID, false, false, groups)
		}
	}

	if update_parent {
		if pj != nil {
			regenerateHtml(pj.Job_ID, false, false, groups)
		} else {
			RegenerateRootHtml()
		}

	}
	g := *groups
	if g[j.Job_Group_Name] > 0 {
		g[j.Job_Group_Name]++
	} else {
		g[j.Job_Group_Name] = 1
	}
}

func RegenerateJobHtml(job_id string) {
	groups := make(map[string]int)
	regenerateHtml(job_id, true, true, &groups)
	for group_name, count := range groups {
		fmt.Printf("Regenerating group %s with %d jobs\n", group_name, count)
		RegenerateGroupHtml(group_name)
	}
	RegenerateActiveHtml()
	RegenerateRootHtml()
}

func StartingJob(job_id string) {
	RegenerateJobHtml(job_id)
}

func FinishedJob(job_id string) {
	RegenerateJobHtml(job_id)
}

func RegenerateAllHtml() {
	fmt.Printf("Regenerating all jobs HTML...\n")
	BatchHtmlRegenerateStart()
	groups := make(map[string]int)
	db := DbOpen()
	defer DbClose(&db)

	iter, _ := db.db.Model(&Job{}).Order("started_at desc").Rows()
	for iter.Next() {
		var elem Job
		db.db.ScanRows(iter, &elem)
		regenerateHtml(elem.Job_ID, false, false, &groups)
	}

	for group_name, count := range groups {
		fmt.Printf("Regenerating group %s with %d jobs\n", group_name, count)
		RegenerateGroupHtml(group_name)
	}
	BatchHtmlRegenerateFinish()
}
